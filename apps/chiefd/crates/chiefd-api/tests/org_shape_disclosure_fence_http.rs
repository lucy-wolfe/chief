//! The org-shape reads are fenced to the caller (B4, routes 2-6).
//!
//! `/v1/org/tree/read`, `/v1/org/tree/structured`, `/v1/org/roster/desired`,
//! `/v1/org/unit/subtree` and `/v1/org/unit/removal-impact` answered every
//! caller with the whole company: every department, every person, and — for
//! the roster — which of them chiefd wants running. Between them they are a
//! complete staff list and org chart, and none of them asked who was calling.
//!
//! # Two fixes, because there are two shapes of route
//!
//! The tree and roster routes name NO target, so the fence is applied to the
//! manifest they render: the fence unit becomes the root, everything outside
//! it is dropped, and the existing projection renders the narrowed manifest
//! unchanged. That is deliberate — a second, fence-aware copy of each
//! projection would be two statements of one rule.
//!
//! The unit routes name their subject in the body, so there is nothing to
//! narrow: the unit is inside the caller's fence or the read is refused.
//!
//! # The Service case is the point, not a nicety
//!
//! `/v1/org/roster/desired` is one of the resident actuator's own calls
//! (`chief-cli/src/actuate/client.rs`). The actuator authenticates as a
//! SERVICE, and `authenticated_person_id` answers `None` for one — so a fence
//! built on that helper would have refused the caller this route exists to
//! serve. Every route below has a service test for that reason.

#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use chiefd_api::authn::middleware::CallerIdentity;
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::CompanyDb;
use chiefd_core::clock::SystemClock;
use chiefd_core::store::identities::{Identity, IdentityKind};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const SLUG: &str = "org-shape-fence";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/org-shape-fence";

/// The COMPOSITE document key the handlers compare against — a display slug
/// fails the label match.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

fn identity(principal: &str, kind: IdentityKind) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind,
        company_slug: match kind {
            IdentityKind::Person => Some(wire_key()),
            _ => None,
        },
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

fn person(principal: &str) -> CallerIdentity {
    identity(principal, IdentityKind::Person)
}

fn service() -> CallerIdentity {
    identity("chiefd-actuator", IdentityKind::Service)
}

async fn app(caller: Option<CallerIdentity>) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let company = Arc::new(
        CompanyDb::open(&wire_key(), &path, Arc::new(SystemClock::default())).expect("company"),
    );
    let mut manifest = chiefd_core::test_support::northstar_manifest(0);
    manifest.slug = SLUG.to_owned();
    let genesis_manifest = manifest.clone();
    company
        .org_manifest_genesis(
            manifest,
            "2026-01-01T00:00:00.000Z".to_owned(),
            chiefd_core::store::person_contracts::build::build_organization_person_contracts(
                &genesis_manifest,
            )
            .expect("person contracts document"),
        )
        .await
        .expect("manifest genesis");
    let feed = Arc::new(ChangeFeed::new());
    let store =
        Arc::new(DocStore::open_with_feed(&path.display().to_string(), 2, feed).expect("docstore"));
    store.ensure_schema().await.expect("schema");
    let router = router_with_supervision_live(
        store,
        1024 * 1024,
        Duration::from_secs(15),
        Some(SupervisionLiveSource::new(company, wire_key())),
    );
    let router = match caller {
        Some(identity) => router.layer(Extension(identity)),
        None => router,
    };
    (router, dir)
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
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn slug_route(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    post(app, uri, json!({ "slug": wire_key() })).await
}

async fn unit_route(app: &axum::Router, uri: &str, unit_id: &str) -> (StatusCode, Value) {
    post(app, uri, json!({ "slug": wire_key(), "unitId": unit_id })).await
}

fn code(body: &Value) -> Option<&str> {
    body.get("code").and_then(Value::as_str)
}

fn rendered(body: &Value) -> String {
    body.to_string()
}

// --- /v1/org/tree/read -----------------------------------------------------

/// THE POSITIVE. The CEO heads the root, so its tree is the whole company.
#[tokio::test]
async fn the_ceo_reads_the_whole_ascii_tree() {
    let (app, _dir) = app(Some(person("chief"))).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/read").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let text = rendered(&body);
    assert!(text.contains("Quant"), "body: {body}");
    assert!(text.contains("IT"), "body: {body}");
}

/// A head's tree is rooted at the unit it heads, so a sibling department is
/// simply absent — the fix is a narrowed MANIFEST, so nothing downstream had
/// to learn about fences.
#[tokio::test]
async fn a_heads_ascii_tree_is_rooted_at_its_own_unit() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/read").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let text = rendered(&body);
    assert!(text.contains("Quant"), "body: {body}");
    assert!(!text.contains("Ira"), "a sibling head must not appear: {body}");
}

#[tokio::test]
async fn a_service_reads_the_whole_ascii_tree() {
    let (app, _dir) = app(Some(service())).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/read").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(rendered(&body).contains("IT"), "body: {body}");
}

#[tokio::test]
async fn a_stranger_is_refused_the_ascii_tree() {
    let (app, _dir) = app(Some(person("stranger"))).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/read").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "body: {body}");
}

// --- /v1/org/tree/structured -----------------------------------------------

#[tokio::test]
async fn the_ceo_reads_the_structured_tree_rooted_at_the_company() {
    let (app, _dir) = app(Some(person("chief"))).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/structured").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.get("rootDepartmentId").and_then(Value::as_str),
        Some("executive"),
        "body: {body}"
    );
    assert!(rendered(&body).contains("it-head"), "body: {body}");
}

/// The BROWSER's tree agrees with the terminal's: same fence, same root, and
/// no person from outside it.
#[tokio::test]
async fn a_heads_structured_tree_is_rooted_at_its_own_unit() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/structured").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body.get("rootDepartmentId").and_then(Value::as_str), Some("quant"), "body: {body}");
    let text = rendered(&body);
    assert!(text.contains("signal-researcher"), "its own report must be present: {body}");
    assert!(!text.contains("it-head"), "a sibling head must not appear: {body}");
}

#[tokio::test]
async fn a_service_reads_the_structured_tree_rooted_at_the_company() {
    let (app, _dir) = app(Some(service())).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/structured").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body.get("rootDepartmentId").and_then(Value::as_str),
        Some("executive"),
        "body: {body}"
    );
}

/// NO CALLER, NO ORG CHART. A fence that let an uncredentialed request through
/// would hand out the whole company, which is the exact disclosure this file
/// exists to close.
#[tokio::test]
async fn without_a_caller_the_structured_tree_is_401() {
    let (app, _dir) = app(None).await;
    let (status, body) = slug_route(&app, "/v1/org/tree/structured").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(code(&body), Some("caller-unauthenticated"), "body: {body}");
}

// --- /v1/org/roster/desired ------------------------------------------------

fn roster_person_ids(body: &Value) -> Vec<String> {
    body.get("people")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("personId").or_else(|| row.get("id")))
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// THE CALLER THIS ROUTE EXISTS TO SERVE. The resident actuator reads the
/// roster to decide who should be running; a fence that resolved a person
/// would have answered it `None` and refused it, stopping the whole company.
#[tokio::test]
async fn the_actuators_service_identity_reads_the_whole_roster() {
    let (app, _dir) = app(Some(service())).await;
    let (status, body) = slug_route(&app, "/v1/org/roster/desired").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(roster_person_ids(&body).len(), 4, "body: {body}");
    assert_eq!(
        body.get("rootDepartmentId").and_then(Value::as_str),
        Some("executive"),
        "body: {body}"
    );
}

#[tokio::test]
async fn the_ceo_reads_the_whole_roster() {
    let (app, _dir) = app(Some(person("chief"))).await;
    let (status, body) = slug_route(&app, "/v1/org/roster/desired").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(roster_person_ids(&body).len(), 4, "body: {body}");
}

/// A head's roster is its own subtree — the staff list stops being a company
/// directory for everybody who is not the CEO.
#[tokio::test]
async fn a_heads_roster_holds_only_its_own_subtree() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = slug_route(&app, "/v1/org/roster/desired").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let mut people = roster_person_ids(&body);
    people.sort();
    assert_eq!(people, vec!["quant-head", "signal-researcher"], "body: {body}");
}

/// NO CALLER, NO STAFF LIST — and no partial one either: the refusal carries
/// no person rows at all, so an uncredentialed caller learns nothing about who
/// works here.
#[tokio::test]
async fn without_a_caller_the_roster_is_401() {
    let (app, _dir) = app(None).await;
    let (status, body) = slug_route(&app, "/v1/org/roster/desired").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(code(&body), Some("caller-unauthenticated"), "body: {body}");
    assert!(roster_person_ids(&body).is_empty(), "body: {body}");
}

// --- /v1/org/unit/subtree and /v1/org/unit/removal-impact ------------------

#[tokio::test]
async fn a_head_reads_the_subtree_of_its_own_unit() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/subtree", "quant").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(rendered(&body).contains("quant"), "body: {body}");
}

/// THE REFUSAL. The subject is named in the body, so there is nothing to
/// narrow: a sibling's unit is refused outright.
#[tokio::test]
async fn a_head_reading_a_siblings_subtree_is_refused() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/subtree", "it").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "body: {body}");

    // The refusal names the subtree and never a job title (2026-08-13 ruling).
    let detail = body.get("detail").and_then(Value::as_str).unwrap_or_default();
    assert!(detail.contains("quant"), "must name the subtree: {detail}");
    for title in ["manager", "head-level", "CEO-level"] {
        assert!(!detail.contains(title), "must never name a job title: {detail}");
    }
}

#[tokio::test]
async fn a_service_reads_any_units_subtree() {
    let (app, _dir) = app(Some(service())).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/subtree", "it").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// The removal impact names the PEOPLE a removal would offboard, so it is a
/// roster disclosure as well as a structural one.
#[tokio::test]
async fn a_head_reading_a_siblings_removal_impact_is_refused() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/removal-impact", "it").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-scope"), "body: {body}");
}

#[tokio::test]
async fn a_head_reads_the_removal_impact_of_its_own_unit() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/removal-impact", "quant").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn a_service_reads_any_units_removal_impact() {
    let (app, _dir) = app(Some(service())).await;
    let (status, body) = unit_route(&app, "/v1/org/unit/removal-impact", "it").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// BOTH unit routes, not just the first: they name their subject in the body,
/// so an uncredentialed caller that got through either one would read a unit it
/// never had to be inside the fence of.
#[tokio::test]
async fn without_a_caller_the_unit_routes_are_401() {
    let (app, _dir) = app(None).await;
    for path in ["/v1/org/unit/subtree", "/v1/org/unit/removal-impact"] {
        let (status, body) = unit_route(&app, path, "it").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} body: {body}");
        assert_eq!(code(&body), Some("caller-unauthenticated"), "{path} body: {body}");
    }
}
