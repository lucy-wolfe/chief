//! E7-S8 live-route tests for `/v1/org/api-host-launch-profile/read`.
//!
//! These use a real CompanyDb and the injected API-host profile source. The
//! profile itself is read from materialized fixtures, never manufactured in the
//! router, so the tests also prove the route cannot silently reconstruct a
//! second host authority.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use chiefd_host::converge_apply::{safety, ApiHostLaunchProfileConfig, ApiHostLaunchProfileSource};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const LABEL: &str = "cobalt";
const SLUG: &str = "cobalt@api-host-profile";
const PATH: &str = "/v1/org/api-host-launch-profile/read";

struct ProfileHarness {
    app: axum::Router,
    slug: String,
    company: Arc<CompanyDb>,
    // Keep this last: the router and CompanyDb release their SQLite handles
    // before the fixture directory removes the backing files.
    _dir: tempfile::TempDir,
}

async fn profile_harness() -> ProfileHarness {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let mut manifest = chiefd_core::test_support::northstar_manifest(0);
    manifest.slug = LABEL.to_owned();
    {
        let mut connection = chiefd_core::store::open_company_db(&path).expect("open seed db");
        let transaction = connection.transaction().expect("seed transaction");
        chiefd_core::store::organization_rows::genesis(&transaction, LABEL, &manifest)
            .expect("seed manifest");
        transaction.commit().expect("commit seed manifest");
    }
    let company = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    let config = profile_config(dir.path());
    materialize_every_person(&manifest, &config);
    let profile_source = ApiHostLaunchProfileSource::new(Arc::clone(&company), config);
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_owned())
        .with_api_host_launch_profile(profile_source);
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live));
    ProfileHarness { app, slug: SLUG.to_owned(), company, _dir: dir }
}

// Owned, not `Box::leak`ed. Leaking kept the directory alive for the router
// that borrows its path and never ran the destructor, so each call left a
// ~1 MB `chief.db` in `TMPDIR` for ever; returning it gives it the calling
// test's stack frame, which outlives the router by construction.
async fn app_without_profile_source() -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(company, SLUG.to_owned());
    (
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live)),
        SLUG.to_owned(),
        dir,
    )
}

fn profile_config(root: &Path) -> ApiHostLaunchProfileConfig {
    let surface_bound = Arc::new(OnceLock::new());
    surface_bound.set(()).expect("latch the surface once");
    ApiHostLaunchProfileConfig {
        dir: root.to_path_buf(),
        home: root.join("operator-home"),
        root_pi_agent_dir: root.join("operator-pi-agent"),
        launcher_root: root.join("launcher"),
        surface_bound,
    }
}

/// Give every non-Chief agent the home the launch gate checks, as
/// `ensure_agent_home` writes it: one folder at
/// `<dir>/.chief/agent/<id>/` holding a real
/// `sessions/` and, among other links, a SYMLINKED `auth.json`.
///
/// The link is part of the fixture on purpose. The gate used to `symlink_
/// metadata` five directories and refuse any symlink, and a fixture of plain
/// directories would satisfy that old rule too — so it would prove nothing
/// about the change. It also holds a real secret behind the link, because the
/// assertion this file cares most about is that the profile never carries one.
fn materialize_every_person(
    manifest: &chiefd_core::store::organization::OrganizationManifest,
    config: &ApiHostLaunchProfileConfig,
) {
    write(
        &config.root_pi_agent_dir.join("auth.json"),
        r#"{"openrouter":{"type":"api_key","key":"fixture-operator-secret"}}"#,
    );
    for person_id in &manifest.people_order {
        if manifest.chief_person_id().is_ok_and(|chief| chief == person_id) {
            write(&chiefd_host::agent_home::chief_identity_key_path(&config.dir), "chief-key");
            continue;
        }
        let home = chiefd_host::agent_home::agent_home(&config.dir, person_id);
        fs::create_dir_all(home.join("sessions")).expect("sessions");
        std::os::unix::fs::symlink(
            config.root_pi_agent_dir.join("auth.json"),
            home.join("auth.json"),
        )
        .expect("the credential link");
    }
}

fn write(path: &Path, contents: &str) {
    chiefd_host::files::publish_atomically(path, contents, 0o644).expect("write fixture");
}

async fn post(app: &axum::Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let payload = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, payload)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_route_admits_shadow_and_returns_the_camel_case_nonsecret_wire_shape() {
    let harness = profile_harness().await;

    let (status, out) = post(&harness.app, json!({"slug": harness.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // Who is actuating rides on every answer, as a fact.
    assert_eq!(out["actuation"]["effectiveMode"], json!("shadow"), "{out}");
    assert_eq!(out["actuation"]["configuredMode"], json!("shadow"), "{out}");
    assert_eq!(out["actuation"]["breakerTripped"], json!(false), "{out}");
    let plans = out["plans"].as_array().expect("plans array");
    assert_eq!(plans.len(), 4, "one materialized profile per person");
    let ceo = &plans[0];
    assert_eq!(ceo["personId"], json!("chief"));
    assert_eq!(ceo["cwd"], json!(harness._dir.path().display().to_string()));
    // NOBODY IS REDIRECTED any more (#1307): chief does not set
    // `PI_CODING_AGENT_DIR` for the Chief or for anybody else, so Pi resolves
    // the operator's own `~/.pi/agent` by its own inheritance. The Chief's
    // session store is the operator's own, named explicitly because every
    // other person's is redirected.
    assert!(ceo["env"].get("PI_CODING_AGENT_DIR").is_none(), "{ceo}");
    assert_eq!(
        ceo["env"]["PI_CODING_AGENT_SESSION_DIR"],
        json!(harness._dir.path().join("operator-pi-agent").join("sessions").display().to_string())
    );
    assert!(ceo["sessionFile"].is_null(), "the Chief has no managed session: {ceo}");
    assert!(
        !harness._dir.path().join(".chief/agent/chief").exists(),
        "the API fixture must prove that the Chief needs no managed home"
    );
    let non_chief =
        plans.iter().find(|plan| plan["personId"] == "signal-researcher").expect("non-Chief plan");
    assert!(
        non_chief["cwd"].as_str().expect("cwd").ends_with(".chief/agent/signal-researcher"),
        "{}",
        non_chief["cwd"]
    );
    assert!(
        non_chief["env"].get("PI_CODING_AGENT_DIR").is_none(),
        "chief must not redirect Pi's config scope for anybody: {non_chief}"
    );
    assert_eq!(
        non_chief["env"]["PI_CODING_AGENT_SESSION_DIR"],
        json!(format!("{}/sessions", non_chief["cwd"].as_str().expect("cwd"))),
        "a non-Chief redirects TRANSCRIPTS to the managed home, and nothing else"
    );
    assert!(ceo.get("person_id").is_none(), "wire must be camelCase only");
    assert!(ceo.get("cliPath").is_none(), "piing owns the sanctioned entry path");
    let env = ceo["env"].as_object().expect("environment object");
    for forbidden in [
        // The retired chiefd-address stamp. A hosted child resolves its own
        // company through beacond, and this host runs MANY companies in one
        // process — there is no value this key could hold that is right for
        // more than one of them, so the wire must never carry it.
        "ORG_CHIEFD_URL",
        "ORG_LAUNCHER_RUNTIME_SOCKET",
        "ORG_LAUNCHER_RUNTIME_SESSION",
        "OPENROUTER_API_KEY",
        "PI_OFFLINE",
    ] {
        assert!(!env.contains_key(forbidden), "profile must not expose {forbidden}");
    }
    assert!(
        !env.values().any(|value| value
            .as_str()
            .is_some_and(|value| value.contains("fixture-operator-secret"))),
        "the operator's own credential must not be serialized: the agent reaches it \
         through a symlink in its own home, and chief never reads it"
    );
    // The facts a hosted agent needs, carried AS facts. They used to be argv
    // (`--tools a,b`, `--session <path>`), which forced the reader to scan a
    // flag list and re-split a comma string to learn them.
    assert!(ceo["tools"].as_array().is_some_and(|tools| !tools.is_empty()), "{ceo}");
    assert!(
        ceo["displayName"].as_str().is_some_and(
            |name| name.starts_with('@') && name.contains(" · Chief Executive Officer")
        ),
        "the person uses one short identity and a real role: {ceo}"
    );
    // Chief is out of the provider/model business AND out of the appearance
    // business: a hosted agent runs as plain Pi on the operator's own defaults,
    // so no route, no registry, no reasoning-effort and no generated theme may
    // appear on this wire at all.
    for retired in ["provider", "model", "thinking", "modelsRegistry", "themes"] {
        assert!(
            ceo.get(retired).is_none(),
            "{retired} must be gone from the hosted-profile wire: {ceo}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_route_fences_a_foreign_company() {
    let harness = profile_harness().await;

    let (status, out) = post(&harness.app, json!({"slug": "someone-else@other"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    assert_eq!(out["code"], json!("unknown-company"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_route_reports_the_absent_live_source_as_unavailable() {
    let (app, slug, _dir) = app_without_profile_source().await;

    let (status, out) = post(&app, json!({"slug": slug})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{out}");
    assert_eq!(out["code"], json!("api-host-launch-profile-unavailable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_route_rejects_malformed_or_unmodeled_requests_before_any_projection() {
    let harness = profile_harness().await;
    let malformed = Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .expect("request");
    let response = harness.app.clone().oneshot(malformed).await.expect("route");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (status, _out) =
        post(&harness.app, json!({"slug": harness.slug, "unexpected": "must refuse"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// The inversion of `profile_route_refuses_apply_mode_to_prevent_a_second_live_host`.
///
/// That test asserted a REFUSAL under `apply`, so that a route consumer could
/// not stand up a second actuator beside the live runtime convergence. The rule it
/// protected is still exactly right — it is just not chiefd's to enforce on a
/// READ. Reading a fact is not actuating on it, and the only party that knows
/// whether it is about to actuate is the reader. The gate also made this
/// contract unreadable by the client that needs it most: a operator client runs
/// against a company in `apply` by definition (#751/P4).
///
/// So the three facts the refusal carried are published instead, and
/// `apps/web` raises the same 409 `company-not-api-hosted` from them —
/// `apps/web/test/server/HostedRoster.test.ts` holds that half.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_route_publishes_apply_mode_as_a_fact_instead_of_refusing_it() {
    let harness = profile_harness().await;
    safety::set_actuation_config(&harness.company, safety::ActuationMode::Apply, false, false)
        .await
        .expect("switch fixture to apply");

    let (status, out) = post(&harness.app, json!({"slug": harness.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["actuation"]["effectiveMode"], json!("apply"), "{out}");
    assert_eq!(out["actuation"]["configuredMode"], json!("apply"), "{out}");
    assert_eq!(out["actuation"]["breakerTripped"], json!(false), "{out}");
    assert!(!out["plans"].as_array().expect("plans array").is_empty(), "{out}");
}

/// A tripped breaker forces the EFFECTIVE mode to shadow while the CONFIGURED
/// one still says apply. The two used to be reported only inside a refusal
/// message; they are now separate fields, so a caller can tell "the operator
/// chose this" from "the breaker forced it" without parsing a sentence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tripped_breaker_shows_as_effective_shadow_over_a_configured_apply() {
    let harness = profile_harness().await;
    safety::set_actuation_config(&harness.company, safety::ActuationMode::Apply, false, false)
        .await
        .expect("switch fixture to apply");
    // The breaker trips on consecutive apply-cycle failures; there is no
    // "trip it" verb, so the fixture produces the failures. Bounded, and it
    // asserts the trip actually happened rather than assuming a count.
    let mut tripped = false;
    for _ in 0..16 {
        if matches!(
            safety::record_cycle_outcome(&harness.company, false)
                .await
                .expect("record a failed cycle"),
            chiefd_host::converge_apply::safety::BreakerAction::Tripped
        ) {
            tripped = true;
            break;
        }
    }
    assert!(tripped, "the fixture must actually trip the breaker");

    let (status, out) = post(&harness.app, json!({"slug": harness.slug})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["actuation"]["effectiveMode"], json!("shadow"), "{out}");
    assert_eq!(out["actuation"]["configuredMode"], json!("apply"), "{out}");
    assert_eq!(out["actuation"]["breakerTripped"], json!(true), "{out}");
}
