//! The eight-route HTTP surface. No authentication (see the crate module
//! doc): any local process may register or deregister any directory.
//! Handlers are thin: validate -> call [`Registry`] -> map the result to a
//! response.
//!
//! **Every route names a company by its DIRECTORY.** There is no route that
//! takes a slug, because a slug names no company: two directories may hold
//! companies called the same thing.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::store::{Admission, Registry, RegistryError};
use crate::wire::{
    is_company_key, CreateCompanyRequest, DeleteCompanyRequest, DeregisterRequest,
    HeartbeatRequest, LookupQuery, RegisterRequest,
};

/// The `{code, message}` refusal shape every non-2xx body uses, matching
/// chiefd's own convention. Callers branch on `code`, never on `message`.
#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

fn error_response(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    (status, Json(ErrorBody { code, message: message.into() })).into_response()
}

fn bad_request(message: impl Into<String>) -> Response {
    error_response(StatusCode::BAD_REQUEST, "bad-request", message)
}

/// The one check every route runs on the `dir` it was given.
///
/// A company directory is an ABSOLUTE canonical path. beacond cannot
/// canonicalise it — resolving a path is the caller's own cwd's business, and
/// the directory may already be deleted — but a relative path is not an
/// identity under any resolution, and admitting one would key one company
/// under as many spellings as callers. This is the surviving half of the
/// deleted data-root policy, which refused the same thing for the same
/// reason.
fn refuse_unusable_dir(dir: &str) -> Option<Response> {
    if dir.trim().is_empty() {
        return Some(bad_request("dir must not be empty"));
    }
    if !std::path::Path::new(dir).is_absolute() {
        return Some(bad_request(format!("dir must be an absolute path, got \"{dir}\"")));
    }
    None
}

/// WHAT THIS BEACOND IS, as beacond itself measured it at start.
///
/// # Why the process REPORTS this rather than letting a caller find out
///
/// `chief` has to answer "is the running beacond the build that is installed
/// right now" — the operator's ruling after a release left `chiefd`, `beacond`
/// and an actuator on old binaries while the fixed ones sat on disk. Every
/// other way of answering it is a caller going behind this process's back:
/// `/proc/<pid>/exe` does not exist on macOS, and the Darwin call that looks
/// equivalent (`PROC_PIDVNODEPATHINFO`) answers with the process's cwd and root
/// vnodes rather than its executable. A process can always stat its OWN
/// executable, everywhere, at a moment when the file certainly still exists.
///
/// So beacond says what it is, exactly as a company daemon says so on its
/// rendezvous, and one rule covers both platforms with one test.
///
/// # And its pid, which nothing else on this box knows
///
/// `chief` spawns beacond DETACHED and drops the child handle — deliberately,
/// because beacond outlives the command that noticed it was missing. There is
/// no pid file and no rendezvous. So before this field existed, the pid of the
/// beacond a box is running was knowable to nobody, and a rule that says
/// "signal the pid it REPORTS, never one you inferred" had nothing to read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeacondFacts {
    /// This process's pid.
    pub pid: u32,
    /// The version this binary was built as.
    pub version: &'static str,
    /// The database file this process opened, absolute.
    ///
    /// Folded in here rather than waiting for its own change: an operator
    /// debugging a registry that "lost" a company needs to know WHICH file
    /// answered, and two beaconds on one box with different `CHIEF_BEACOND_DB`
    /// values look identical from outside without it.
    pub db_path: String,
    /// Which file this process is running, for the installed-build check.
    /// `None` only if this process cannot stat its own executable.
    pub build: Option<host_primitives::rendezvous::ReportedBuild>,
}

impl BeacondFacts {
    /// Measure them, once, at start.
    #[must_use]
    pub fn measure(db_path: String) -> Self {
        Self {
            pid: std::process::id(),
            version: env!("CHIEF_VERSION"),
            db_path,
            build: host_primitives::rendezvous::ReportedBuild::of_running_process(),
        }
    }
}

/// The router's state: the registry every route already had, plus the facts
/// only `/v1/health` reads.
///
/// A substate rather than a new parameter on eight handlers: `FromRef` lets
/// every existing route keep taking `State<Arc<Registry>>` untouched, so this
/// change is visible exactly where it is real.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<Registry>,
    facts: Arc<BeacondFacts>,
}

impl axum::extract::FromRef<AppState> for Arc<Registry> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.registry)
    }
}

impl axum::extract::FromRef<AppState> for Arc<BeacondFacts> {
    fn from_ref(state: &AppState) -> Self {
        Arc::clone(&state.facts)
    }
}

/// Build the beacond router over a shared [`Registry`] and this process's own
/// facts.
pub fn router(registry: Arc<Registry>, facts: BeacondFacts) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/lookup", get(lookup))
        .route("/v1/list", get(list))
        .route("/v1/company/create", post(create_company))
        .route("/v1/company/delete", post(delete_company))
        .route("/v1/register", post(register))
        .route("/v1/heartbeat", post(heartbeat))
        .route("/v1/deregister", post(deregister))
        .with_state(AppState { registry, facts: Arc::new(facts) })
}

/// The health answer: whether beacond can serve, and WHAT beacond is.
///
/// The identity fields ride the health answer rather than a route of their own
/// because every caller that wants them is already asking this: `chief`'s
/// discovery ladder probes health before it decides whether to spawn, so the
/// installed-build check costs no extra request. They are reported on the
/// UNHEALTHY answer too — a beacond whose schema is missing is exactly the one
/// an operator needs to identify.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthOk {
    status: &'static str,
    #[serde(flatten)]
    facts: BeacondFacts,
}

async fn health(
    State(registry): State<Arc<Registry>>,
    State(facts): State<Arc<BeacondFacts>>,
) -> Response {
    let body = |status: &'static str| Json(HealthOk { status, facts: (*facts).clone() });
    match registry.schema_ready() {
        Ok(true) => (StatusCode::OK, body("ok")).into_response(),
        Ok(false) => (StatusCode::SERVICE_UNAVAILABLE, body("schema-missing")).into_response(),
        Err(error) => {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "schema-missing", error.to_string())
        }
    }
}

/// `GET /v1/lookup?dir=…` — one company, by the directory it occupies.
///
/// A miss carries no suggestion and no message. Its predecessor did: it took
/// a slug the operator had TYPED (`chief attach <slug>`), so "did you mean
/// cobalt?" was worth computing. A caller does not type a directory — it
/// sends the one it is standing in — so a miss means "no company is
/// registered here", and the nearest OTHER directory on the box is not an
/// answer to anything. The Levenshtein machinery that served it is deleted
/// with `directory.rs`.
#[derive(Serialize)]
struct LookupResponse {
    found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    company: Option<crate::wire::Company>,
}

async fn lookup(
    State(registry): State<Arc<Registry>>,
    Query(query): Query<LookupQuery>,
) -> Response {
    if let Some(refusal) = refuse_unusable_dir(&query.dir) {
        return refusal;
    }
    match registry.lookup(&query.dir) {
        Ok(Some(company)) => {
            (StatusCode::OK, Json(LookupResponse { found: true, company: Some(company) }))
                .into_response()
        }
        Ok(None) => {
            (StatusCode::OK, Json(LookupResponse { found: false, company: None })).into_response()
        }
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct ListResponse {
    companies: Vec<crate::wire::Company>,
}

async fn list(State(registry): State<Arc<Registry>>) -> Response {
    match registry.list() {
        Ok(companies) => (StatusCode::OK, Json(ListResponse { companies })).into_response(),
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct CreateResponse {
    created: bool,
    company: crate::wire::Company,
}

/// `POST /v1/company/create` — record the company that occupies a directory.
///
/// **There is no conflict arm.** Its predecessor answered `409 slug-taken`
/// when the slug existed under a different orgs root, because one slug under
/// two roots was two companies fighting over one key. The directory is the
/// key now, so the same pair is simply two rows, and the only thing a second
/// create can have changed is the display word — which it updates.
#[tracing::instrument(name = "beacond.company.create", skip_all, fields(company = %request.dir))]
async fn create_company(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<CreateCompanyRequest>,
) -> Response {
    tracing::info!(
        event = "beacond.company.create.request",
        slug = %request.slug,
        key = %request.key,
        "a company claim was requested"
    );
    if let Some(refusal) = refuse_unusable_dir(&request.dir) {
        return refusal;
    }
    if request.slug.trim().is_empty() {
        return bad_request("slug must not be empty");
    }
    if !is_company_key(&request.key) {
        return bad_request(format!(
            "key must be sha256(dir)[..12] — twelve lowercase hex characters, got \"{}\"",
            request.key
        ));
    }
    let now = crate::wire::now_iso_millis();
    match registry.create(&request.dir, &request.key, &request.slug, &now) {
        Ok((company, created)) => {
            (StatusCode::OK, Json(CreateResponse { created, company })).into_response()
        }
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct DeleteResponse {
    deleted: bool,
}

#[tracing::instrument(name = "beacond.company.delete", skip_all, fields(company = %request.dir))]
async fn delete_company(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<DeleteCompanyRequest>,
) -> Response {
    if let Some(refusal) = refuse_unusable_dir(&request.dir) {
        return refusal;
    }
    match registry.delete(&request.dir) {
        Ok(deleted) => (StatusCode::OK, Json(DeleteResponse { deleted })).into_response(),
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct RegisterResponse {
    registered: bool,
    company: crate::wire::Company,
}

#[derive(Serialize)]
struct OccupiedBody {
    code: &'static str,
    message: String,
    pid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(rename = "lastSeenAt", skip_serializing_if = "Option::is_none")]
    last_seen_at: Option<String>,
}

fn valid_url_scheme(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// The other end of the wait a launching client sits in.
///
/// The incident's whole reconstruction turned on ONE fact — the exact instant
/// beacond admitted the new daemon — and that fact had to be recovered from a
/// Pi session transcript because beacond wrote it nowhere. It writes it here.
#[tracing::instrument(name = "beacond.register", skip_all, fields(company = %request.dir))]
async fn register(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<RegisterRequest>,
) -> Response {
    tracing::info!(
        event = "beacond.register.request",
        pid = request.pid,
        port = request.port,
        "a daemon asked to be admitted"
    );
    if let Some(refusal) = refuse_unusable_dir(&request.dir) {
        return refusal;
    }
    if request.port == 0 {
        return bad_request("port must not be 0");
    }
    if request.pid < 1 {
        return bad_request("pid must be a positive integer");
    }
    if !valid_url_scheme(&request.url) {
        return bad_request("url must be http:// or https://");
    }

    let location = crate::wire::Location {
        dir: request.dir.clone(),
        url: request.url,
        port: request.port,
        pid: request.pid,
        hostname: request.hostname,
    };

    match registry.register(&location) {
        Ok(Admission::Admitted(company)) => {
            (StatusCode::OK, Json(RegisterResponse { registered: true, company })).into_response()
        }
        Ok(Admission::Occupied { pid, hostname, last_seen_at }) => (
            StatusCode::CONFLICT,
            Json(OccupiedBody {
                code: "company-live-elsewhere",
                message: format!(
                    "the company in \"{}\" is already served by pid {pid}",
                    request.dir
                ),
                pid,
                hostname,
                last_seen_at,
            }),
        )
            .into_response(),
        Err(RegistryError::UnknownCompany) => error_response(
            StatusCode::NOT_FOUND,
            "unknown-company",
            format!("no company is registered in \"{}\"", request.dir),
        ),
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct HeartbeatResponse {
    ok: bool,
    #[serde(rename = "lastSeenAt")]
    last_seen_at: String,
}

#[derive(Serialize)]
struct PidMismatchBody {
    code: &'static str,
    message: String,
    #[serde(rename = "recordedPid")]
    recorded_pid: i64,
}

async fn heartbeat(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<HeartbeatRequest>,
) -> Response {
    if let Some(refusal) = refuse_unusable_dir(&request.dir) {
        return refusal;
    }
    let now = crate::wire::now_iso_millis();
    match registry.heartbeat(&request.dir, request.pid, &now) {
        Ok(Some(company)) => (
            StatusCode::OK,
            Json(HeartbeatResponse { ok: true, last_seen_at: company.last_seen_at.unwrap_or(now) }),
        )
            .into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "unknown-company",
            format!("no company is registered in \"{}\"", request.dir),
        ),
        Err(RegistryError::PidMismatch { recorded_pid }) => (
            StatusCode::CONFLICT,
            Json(PidMismatchBody {
                code: "pid-mismatch",
                message: format!(
                    "the company in \"{}\" is recorded under pid {recorded_pid}",
                    request.dir
                ),
                recorded_pid,
            }),
        )
            .into_response(),
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[derive(Serialize)]
struct DeregisterResponse {
    deregistered: bool,
}

#[tracing::instrument(name = "beacond.deregister", skip_all, fields(company = %request.dir))]
async fn deregister(
    State(registry): State<Arc<Registry>>,
    Json(request): Json<DeregisterRequest>,
) -> Response {
    if let Some(refusal) = refuse_unusable_dir(&request.dir) {
        return refusal;
    }
    match registry.deregister(&request.dir, request.pid) {
        Ok(deregistered) => {
            (StatusCode::OK, Json(DeregisterResponse { deregistered })).into_response()
        }
        Err(RegistryError::PidMismatch { recorded_pid }) => (
            StatusCode::CONFLICT,
            Json(PidMismatchBody {
                code: "pid-mismatch",
                message: format!(
                    "the company in \"{}\" is recorded under pid {recorded_pid}",
                    request.dir
                ),
                recorded_pid,
            }),
        )
            .into_response(),
        Err(error) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "store-error", error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;

    fn test_router() -> (tempfile::TempDir, Router) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beacond.sqlite");
        let registry = Arc::new(Registry::open(&path).expect("open"));
        let facts = BeacondFacts::measure(path.display().to_string());
        (dir, router(registry, facts))
    }

    async fn send(
        app: Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = if let Some(body) = body {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).expect("serialize body")))
                .expect("build request")
        } else {
            Request::builder().method(method).uri(uri).body(Body::empty()).expect("build request")
        };
        let response = app.oneshot(request).await.expect("router response");
        let status = response.status();
        let bytes = response.into_body().collect().await.expect("read body").to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("parse body as JSON")
        };
        (status, json)
    }

    /// The `{dir, key, slug}` a claim carries. The key is of the right shape
    /// (twelve lowercase hex); its value is the caller's business.
    fn claim(dir: &str, slug: &str) -> Value {
        json!({"dir": dir, "key": "0123456789ab", "slug": slug})
    }

    // ---- test 12: health ----

    #[tokio::test]
    async fn health_is_ok_once_the_schema_exists() {
        let (_dir, app) = test_router();
        let (status, body) = send(app, "GET", "/v1/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], json!("ok"));
    }

    /// HEALTH SAYS WHAT THIS BEACOND IS, not only that it is well.
    ///
    /// `chief` compares the reported build against the installed one to decide
    /// whether the running beacond is the build on disk, and it signals the pid
    /// beacond REPORTS rather than one it inferred — there is no other source
    /// for either, because `chief` spawns beacond detached and drops the child
    /// handle. The db path rides along for the operator: two beaconds on one
    /// box with different `CHIEF_BEACOND_DB` values are indistinguishable from
    /// outside without it.
    #[tokio::test]
    async fn health_reports_the_pid_version_database_and_build_of_this_process() {
        let (dir, app) = test_router();
        let (status, body) = send(app, "GET", "/v1/health", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["pid"], json!(std::process::id()), "its own pid, never an inferred one");
        assert_eq!(body["version"], json!(env!("CHIEF_VERSION")));
        assert_eq!(
            body["dbPath"],
            json!(dir.path().join("beacond.sqlite").display().to_string()),
            "the file this process actually opened"
        );
        let build = &body["build"];
        assert!(
            build["exe"].as_str().is_some_and(|exe| !exe.is_empty()),
            "the executable this process is running: {body}"
        );
        assert!(
            build["identity"]["ino"].as_u64().is_some_and(|ino| ino > 0),
            "a real inode, measured while the file still exists: {body}"
        );
        assert!(
            build["identity"]["size"].as_u64().is_some_and(|size| size > 0),
            "and a real size, because an inode number alone can be reused: {body}"
        );
    }

    /// AND IT SAYS SO WHEN IT IS NOT WELL. A beacond whose schema is missing is
    /// exactly the one an operator has to identify, so the identity must not be
    /// the reward for being healthy.
    #[tokio::test]
    async fn an_unhealthy_beacond_still_says_which_beacond_it_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beacond.sqlite");
        let registry = Arc::new(Registry::open(&path).expect("open"));
        // Dropped through a SECOND connection to the same file rather than by
        // adding a test-only mutator to `Registry`: production API exists for
        // production, and this test needs a state the product never creates.
        //
        // The `Connection::open` ban defends ONE seam — no code opening a
        // COMPANY connection behind the writer actor's back — and beacond's
        // registry is not a company database at all. Same exception, same
        // reason, as `store.rs`'s own open a few lines into this crate.
        #[allow(clippy::disallowed_methods)]
        rusqlite::Connection::open(&path)
            .expect("second connection")
            .execute("DROP TABLE companies", [])
            .expect("drop the schema out from under the registry");
        let facts = BeacondFacts::measure(path.display().to_string());
        let (status, body) = send(router(registry, facts), "GET", "/v1/health", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], json!("schema-missing"));
        assert_eq!(body["pid"], json!(std::process::id()), "still names itself: {body}");
    }

    // ---- test 11: the eight routes, documented status + body shape ----

    /// EVERY route refuses a directory that is not an absolute path.
    ///
    /// One assertion per route rather than one for the helper: the check has
    /// to be WIRED into each handler, and a route that forgot to call it is
    /// exactly the defect this pins. A relative dir would key one company
    /// under as many spellings as there are callers' working directories.
    #[tokio::test]
    async fn every_route_refuses_a_relative_or_empty_directory() {
        for (method, uri, body) in [
            ("GET", "/v1/lookup?dir=", None),
            ("GET", "/v1/lookup?dir=work/anvils", None),
            ("POST", "/v1/company/create", Some(claim("work/anvils", "anvils"))),
            ("POST", "/v1/company/delete", Some(json!({"dir": "work/anvils"}))),
            (
                "POST",
                "/v1/register",
                Some(
                    json!({"dir": "work/anvils", "url": "http://127.0.0.1:1", "port": 1, "pid": 1, "hostname": "box"}),
                ),
            ),
            ("POST", "/v1/heartbeat", Some(json!({"dir": "work/anvils", "pid": 1}))),
            ("POST", "/v1/deregister", Some(json!({"dir": "", "pid": 1}))),
        ] {
            let (_dir, app) = test_router();
            let (status, response) = send(app, method, uri, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}");
            assert_eq!(response["code"], json!("bad-request"), "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn lookup_on_a_never_registered_company_has_no_location_keys_at_all() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let (_status, _body) =
            send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;

        let (status, body) = send(app2, "GET", "/v1/lookup?dir=/work/acme", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["found"], json!(true));
        let company = body["company"].as_object().expect("company object");
        assert_eq!(company["dir"], json!("/work/acme"));
        assert_eq!(company["key"], json!("0123456789ab"));
        assert_eq!(company["slug"], json!("acme"));
        assert!(!company.contains_key("url"));
        assert!(!company.contains_key("port"));
        assert!(!company.contains_key("pid"));
        assert!(!company.contains_key("hostname"));
        assert!(!company.contains_key("lastSeenAt"));
    }

    /// A MISS IS BARE. The suggestion and the operator-facing message that
    /// used to ride here answered a slug an operator had typed; nobody types
    /// a directory, so the nearest other one on the box is not an answer.
    #[tokio::test]
    async fn a_missed_lookup_carries_nothing_but_found_false() {
        let (_dir, app) = test_router();
        let create = app.clone();
        send(create, "POST", "/v1/company/create", Some(claim("/work/cobalt", "cobalt"))).await;

        let (status, body) = send(app, "GET", "/v1/lookup?dir=/work/cobolt", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["found"], json!(false));
        let object = body.as_object().expect("lookup body object");
        assert!(!object.contains_key("company"));
        assert!(!object.contains_key("suggestion"), "a directory is not a typo to correct");
        assert!(!object.contains_key("message"));
    }

    #[tokio::test]
    async fn create_rejects_an_empty_slug_and_a_key_that_is_not_a_directory_hash() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let (status, _) = send(
            app,
            "POST",
            "/v1/company/create",
            Some(json!({"dir": "/work/acme", "key": "0123456789ab", "slug": ""})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // The mistake this catches: a caller filling `key` with the company's
        // NAME, which the old composite key made a plausible slip.
        let (status, body) = send(
            app2,
            "POST",
            "/v1/company/create",
            Some(json!({"dir": "/work/acme", "key": "acme", "slug": "acme"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], json!("bad-request"));
    }

    #[tokio::test]
    async fn create_on_the_same_directory_twice_is_200_created_false() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let (status1, resp1) =
            send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        assert_eq!(status1, StatusCode::OK);
        assert_eq!(resp1["created"], json!(true));

        let (status2, resp2) =
            send(app2, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        assert_eq!(
            status2,
            StatusCode::OK,
            "the identical re-create must not 409 — it breaks `company create`"
        );
        assert_eq!(resp2["created"], json!(false));
    }

    /// TWO DIRECTORIES, ONE NAME, TWO COMPANIES — over HTTP.
    ///
    /// The second claim used to be `409 slug-taken`. There is no such refusal
    /// any more, and there is nothing left in the surface that could produce
    /// one: the pair is two rows the registry lists side by side.
    #[tokio::test]
    async fn two_directories_may_claim_the_same_slug_and_both_are_listed() {
        let (_dir, app) = test_router();
        let second = app.clone();
        let listing = app.clone();
        let (first_status, _) =
            send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        assert_eq!(first_status, StatusCode::OK);

        let (status, body) = send(
            second,
            "POST",
            "/v1/company/create",
            Some(json!({"dir": "/elsewhere/acme", "key": "cafebabe0011", "slug": "acme"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "a same-named company elsewhere is not a conflict");
        assert_eq!(body["created"], json!(true));

        let (status, body) = send(listing, "GET", "/v1/list", None).await;
        assert_eq!(status, StatusCode::OK);
        let companies = body["companies"].as_array().expect("array");
        assert_eq!(companies.len(), 2);
        assert_eq!(companies[0]["dir"], json!("/elsewhere/acme"));
        assert_eq!(companies[1]["dir"], json!("/work/acme"));
    }

    /// A re-create under a new word RENAMES the company in the listing.
    #[tokio::test]
    async fn re_creating_a_directory_under_a_new_slug_renames_it() {
        let (_dir, app) = test_router();
        let rename = app.clone();
        let lookup = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;

        let (status, body) =
            send(rename, "POST", "/v1/company/create", Some(claim("/work/acme", "polaris"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["created"], json!(false));

        let (_status, body) = send(lookup, "GET", "/v1/lookup?dir=/work/acme", None).await;
        assert_eq!(body["company"]["slug"], json!("polaris"));
    }

    #[tokio::test]
    async fn register_on_an_unknown_directory_is_404() {
        let (_dir, app) = test_router();
        let (status, body) = send(
            app,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/ghost", "url": "http://127.0.0.1:1", "port": 1, "pid": 1, "hostname": "box"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], json!("unknown-company"));
    }

    #[tokio::test]
    async fn register_rejects_port_zero_pid_zero_and_a_bad_url_scheme() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let app3 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;

        let (status, _) = send(
            app2.clone(),
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:1", "port": 0, "pid": 1, "hostname": "box"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = send(
            app2,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:1", "port": 1, "pid": 0, "hostname": "box"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = send(
            app3,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "ftp://127.0.0.1:1", "port": 1, "pid": 1, "hostname": "box"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn register_conflict_names_the_incumbent_pid_hostname_and_last_seen_at() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let app3 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        let me = i64::from(std::process::id());
        send(
            app2,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:1", "port": 1, "pid": me, "hostname": "box"})),
        )
        .await;

        let (status, body) = send(
            app3,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:2", "port": 2, "pid": me + 999_999, "hostname": "box"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], json!("company-live-elsewhere"));
        assert_eq!(body["pid"], json!(me));
        assert_eq!(body["hostname"], json!("box"));
        assert!(body.get("lastSeenAt").is_some());
    }

    #[tokio::test]
    async fn heartbeat_on_an_unknown_directory_is_404() {
        let (_dir, app) = test_router();
        let (status, body) =
            send(app, "POST", "/v1/heartbeat", Some(json!({"dir": "/work/ghost", "pid": 1}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], json!("unknown-company"));
    }

    #[tokio::test]
    async fn heartbeat_pid_mismatch_is_409() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let app3 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        send(
            app2,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:1", "port": 1, "pid": 4242, "hostname": "box"})),
        )
        .await;

        let (status, body) =
            send(app3, "POST", "/v1/heartbeat", Some(json!({"dir": "/work/acme", "pid": 9999})))
                .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], json!("pid-mismatch"));
    }

    #[tokio::test]
    async fn deregister_on_an_unknown_directory_is_200_deregistered_false() {
        let (_dir, app) = test_router();
        let (status, body) =
            send(app, "POST", "/v1/deregister", Some(json!({"dir": "/work/ghost", "pid": 1})))
                .await;
        assert_eq!(status, StatusCode::OK, "deregistering an unknown directory is not an error");
        assert_eq!(body["deregistered"], json!(false));
    }

    #[tokio::test]
    async fn deregister_pid_mismatch_is_409() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let app3 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;
        send(
            app2,
            "POST",
            "/v1/register",
            Some(json!({"dir": "/work/acme", "url": "http://127.0.0.1:1", "port": 1, "pid": 4242, "hostname": "box"})),
        )
        .await;

        let (status, body) =
            send(app3, "POST", "/v1/deregister", Some(json!({"dir": "/work/acme", "pid": 9999})))
                .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], json!("pid-mismatch"));
    }

    #[tokio::test]
    async fn list_reflects_created_companies() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;

        let (status, body) = send(app2, "GET", "/v1/list", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["companies"].as_array().expect("array").len(), 1);
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let (_dir, app) = test_router();
        let app2 = app.clone();
        let app3 = app.clone();
        send(app, "POST", "/v1/company/create", Some(claim("/work/acme", "acme"))).await;

        let (status, body) =
            send(app2, "POST", "/v1/company/delete", Some(json!({"dir": "/work/acme"}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], json!(true));

        let (status, body) =
            send(app3, "POST", "/v1/company/delete", Some(json!({"dir": "/work/acme"}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["deleted"], json!(false));
    }

    #[tokio::test]
    async fn every_error_body_parses_as_code_message() {
        let (_dir, app) = test_router();
        let (status, body) =
            send(app, "POST", "/v1/heartbeat", Some(json!({"dir": "/work/ghost", "pid": 1}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["code"].is_string());
        assert!(body["message"].is_string());
    }
}
