//! E7-S3 live-seam integration tests: `/v1/org/settings/{read,publish}` and
//! `/v1/org/person-contracts/projection-plan` over the REAL routes, a real
//! `CompanyDb` and a real `SupervisionLiveSource` — the same harness shape
//! `org_row_seam_b4.rs` uses for the sibling normalized-row ports.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// `CompanyDb::open`'s bare label — the `org_settings`/`person_contracts` row
/// key. Distinct from [`SLUG`], the composite `documentKey` every route
/// requires (mirrors `org_row_seam_b4.rs`'s `"cobalt"` / `"cobalt@b4seam"`
/// split).
const LABEL: &str = "cobalt";
const SLUG: &str = "cobalt@settingsseam";

// Owned, not `Box::leak`ed. Leaking kept the directory alive for the router
// that borrows its path and never ran the destructor, so each call left a
// ~1 MB `chief.db` in `TMPDIR` for ever; returning it gives it the calling
// test's stack frame, which outlives the router by construction.
async fn app_and_slug() -> (axum::Router, String, PathBuf, tempfile::TempDir) {
    let (app, slug, _company, path, dir) = harness().await;
    (app, slug, path, dir)
}

/// The same fixture, plus the live `CompanyDb`. The projection-plan tests need
/// it: `/v1/org/person-contracts/publish` is deleted (the publisher-route
/// sweep found no caller), so contracts are seeded the way production seeds
/// them — in-process, through the same `CompanyDb` method the deleted handler
/// was a one-line pass-through to.
async fn harness() -> (axum::Router, String, Arc<CompanyDb>, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    // Seed a real manifest genesis (which writes `org_settings` as one of its
    // rows, `write_org_settings`) so this seam has a consistent company to
    // read/publish against — an `org_settings` row with no departments is an
    // invalid partial state `CompanyDb::open`'s eager reconstruction refuses.
    {
        let mut conn =
            chiefd_core::store::open_company_db(&path).expect("open company db for seeding");
        let mut manifest = chiefd_core::test_support::northstar_manifest(0);
        manifest.slug = LABEL.to_string();
        let tx = conn.transaction().expect("seed transaction");
        chiefd_core::store::organization_rows::genesis(&tx, LABEL, &manifest)
            .expect("seed genesis");
        tx.commit().expect("commit seed genesis");
    }
    let company = Arc::new(
        CompanyDb::open(LABEL, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_string());
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live));
    (app, SLUG.to_string(), company, path, dir)
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let payload: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, payload)
}

// ---------------------------------------------------------------- settings -

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_publish_then_read_round_trips_launcher_root() {
    let (app, slug, _path, _dir) = app_and_slug().await;
    let (status, out) = post(
        &app,
        "/v1/org/settings/publish",
        json!({"slug": slug, "at": "2026-08-04T00:00:00.000Z", "launcherRoot": "/checkouts/main"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["applied"], json!(true));
    // > 0, not a fixed literal: the seeded genesis above already advanced the
    // shared `org_events` cursor with its own department/person touches.
    assert!(out["seq"].as_i64().expect("seq") > 0, "{out}");

    let (status, out) = post(&app, "/v1/org/settings/read", json!({"slug": slug})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(true));
    assert_eq!(out["settings"]["launcherRoot"], json!("/checkouts/main"));
    // The four policy ints seeded above must round-trip untouched.
    assert_eq!(out["settings"]["supervisionIntervalMs"], json!(900_000));
    assert_eq!(out["settings"]["acknowledgementTimeoutMs"], json!(90_000));
    assert_eq!(out["settings"]["acknowledgementRetryLimit"], json!(1));
    assert_eq!(out["settings"]["replacementLimit"], json!(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_publish_refuses_an_installed_versioned_resource_root() {
    // H.2.5: the override pins a CHECKOUT, never the install. Persisting an
    // exe-derived `…/versions/<v>/resources` root would break the company the
    // moment a later `chief upgrade` pruned that directory, so the publish is
    // refused (422) and nothing is written.
    let (app, slug, _path, _dir) = app_and_slug().await;
    let (status, out) = post(
        &app,
        "/v1/org/settings/publish",
        json!({
            "slug": slug,
            "at": "2026-08-25T00:00:00.000Z",
            "launcherRoot": "/home/me/.chief/versions/2.0.7/resources"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");

    // And the store is untouched — the override stays null.
    let (status, out) = post(&app, "/v1/org/settings/read", json!({"slug": slug})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["settings"]["launcherRoot"], Value::Null, "a refused publish writes nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_read_before_any_publish_reports_launcher_root_null() {
    let (app, slug, _path, _dir) = app_and_slug().await;
    let (status, out) = post(&app, "/v1/org/settings/read", json!({"slug": slug})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(true));
    assert_eq!(out["settings"]["launcherRoot"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_publish_never_mutates_the_four_policy_ints() {
    let (app, slug, _path, _dir) = app_and_slug().await;
    post(
        &app,
        "/v1/org/settings/publish",
        json!({"slug": slug, "at": "t1", "launcherRoot": "/checkouts/a"}),
    )
    .await;
    let (_, before) = post(&app, "/v1/org/settings/read", json!({"slug": slug})).await;
    post(
        &app,
        "/v1/org/settings/publish",
        json!({"slug": slug, "at": "t2", "launcherRoot": "/checkouts/b"}),
    )
    .await;
    let (_, after) = post(&app, "/v1/org/settings/read", json!({"slug": slug})).await;
    assert_eq!(
        before["settings"]["supervisionIntervalMs"],
        after["settings"]["supervisionIntervalMs"]
    );
    assert_eq!(
        before["settings"]["acknowledgementTimeoutMs"],
        after["settings"]["acknowledgementTimeoutMs"]
    );
    assert_eq!(
        before["settings"]["acknowledgementRetryLimit"],
        after["settings"]["acknowledgementRetryLimit"]
    );
    assert_eq!(before["settings"]["replacementLimit"], after["settings"]["replacementLimit"]);
    assert_eq!(after["settings"]["launcherRoot"], json!("/checkouts/b"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_a_foreign_slug_reads_not_found_and_publishes_404() {
    let (app, _slug, _path, _dir) = app_and_slug().await;
    let (status, out) =
        post(&app, "/v1/org/settings/read", json!({"slug": "someone-else@x"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["found"], json!(false));

    let (status, out) = post(
        &app,
        "/v1/org/settings/publish",
        json!({"slug": "someone-else@x", "at": "t", "launcherRoot": "/checkouts/a"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    assert_eq!(out["code"], json!("unknown-company"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_publish_against_a_live_company_with_no_org_settings_row_is_404() {
    // Same own-company slug, but a fresh database with no org_settings row —
    // i.e. genesis never ran. Distinct from the foreign-slug case above:
    // this exercises the store-layer refusal mapping, not the router's
    // own-company gate.
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
    let live = SupervisionLiveSource::new(Arc::clone(&company), SLUG.to_string());
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live));

    let (status, out) = post(
        &app,
        "/v1/org/settings/publish",
        json!({"slug": SLUG, "at": "t", "launcherRoot": "/checkouts/a"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    assert_eq!(out["code"], json!("unknown-company"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_publish_malformed_body_is_a_client_error_not_a_silent_publish() {
    let (app, slug, _path, _dir) = app_and_slug().await;
    // Not valid JSON at all — axum's `Json` extractor rejects a syntax error
    // as 400, before the handler (and this route's own 404/422 mapping) ever
    // runs.
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/settings/publish")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Valid JSON, but missing the required `launcherRoot` field — a data
    // error, not a syntax error; axum's `Json` extractor rejects this with
    // 422, never with a silent empty-string publish.
    let (status, _out) =
        post(&app, "/v1/org/settings/publish", json!({"slug": slug, "at": "t"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------------------- projection plan -

fn contracts_doc(entries: &[(&str, &str)]) -> Value {
    json!({
        "version": 1,
        "organization": LABEL,
        "contracts": entries
            .iter()
            .map(|(id, text)| (id.to_string(), json!({"text": text, "md5": md5_of(text)})))
            .collect::<serde_json::Map<_, _>>(),
    })
}

/// A stand-in "md5" — the store never recomputes it, only string-compares, so
/// any deterministic-per-text function proves the projection-plan logic
/// (mirrors `person_contracts::rows::tests::entry`'s `md5-of-<text>`).
fn md5_of(text: &str) -> String {
    format!("md5-of-{text}")
}

async fn seed_contracts(company: &CompanyDb, entries: &[(&str, &str)]) {
    let doc: chiefd_core::store::person_contracts::rows::OrganizationPersonContracts =
        serde_json::from_value(contracts_doc(entries)).expect("contracts doc");
    company.org_person_contracts_publish("t".to_string(), doc).await.expect("the contracts commit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_keeps_a_matching_observed_md5() {
    let (app, slug, company, _path, _dir) = harness().await;
    seed_contracts(&company, &[("chief", "be the CEO")]).await;

    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({"slug": slug, "observed": [{"personId": "chief", "md5": md5_of("be the CEO")}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["actions"], json!([{"personId": "chief", "action": "keep"}]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_writes_the_stored_text_on_a_stale_md5() {
    let (app, slug, company, _path, _dir) = harness().await;
    seed_contracts(&company, &[("chief", "be the CEO")]).await;

    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({"slug": slug, "observed": [{"personId": "chief", "md5": "stale"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        out["actions"],
        json!([{"personId": "chief", "action": "write", "text": "be the CEO"}])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_writes_when_the_file_is_missing_md5_null() {
    let (app, slug, company, _path, _dir) = harness().await;
    seed_contracts(&company, &[("chief", "be the CEO")]).await;

    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({"slug": slug, "observed": [{"personId": "chief", "md5": null}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        out["actions"],
        json!([{"personId": "chief", "action": "write", "text": "be the CEO"}])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_unknown_person_is_422() {
    let (app, slug, _path, _dir) = app_and_slug().await;
    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({"slug": slug, "observed": [{"personId": "ghost", "md5": null}]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    assert_eq!(out["code"], json!("unknown-person-contract"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_mixed_roster_returns_one_action_per_person_in_request_order() {
    let (app, slug, company, _path, _dir) = harness().await;
    seed_contracts(&company, &[("chief", "be the CEO"), ("ada", "build it")]).await;

    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({
            "slug": slug,
            "observed": [
                {"personId": "ada", "md5": md5_of("build it")},
                {"personId": "chief", "md5": null},
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        out["actions"],
        json!([
            {"personId": "ada", "action": "keep"},
            {"personId": "chief", "action": "write", "text": "be the CEO"},
        ])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projection_plan_a_foreign_slug_is_404() {
    let (app, _slug, _path, _dir) = app_and_slug().await;
    let (status, out) = post(
        &app,
        "/v1/org/person-contracts/projection-plan",
        json!({"slug": "someone-else@x", "observed": []}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{out}");
    assert_eq!(out["code"], json!("unknown-company"));
}
