//! `/v1/org/lifecycle-status/read` derives its scope from the caller (B4).
//!
//! `scopeDepartmentId` was an OPTIONAL, caller-supplied filter. Omitting it
//! returned every department and every person in the company, so the whole
//! control board — who exists, where they sit, and who is up — was one
//! anonymous POST away. That is a filter the caller CHOOSES; a fence is what
//! the server APPLIES, and this route now applies one.
//!
//! # The case this file exists to prove
//!
//! A read must never resolve a person. `authenticated_person_id` answers `None`
//! for a `Service` identity, and the resident actuator authenticates as a
//! service and makes only reads — so a fence built on that helper would
//! authenticate the actuator and then refuse it. The fence here looks for a
//! PERSON ROW instead and engages only when it finds one, which is why
//! `a_service_identity_is_admitted_and_sees_the_whole_company` is not a nicety
//! in this file: it is the regression the design is aimed at.
//!
//! Five shapes, and the positives keep the refusal honest — a route that
//! refused everybody would satisfy the negative on its own:
//!
//! * the CEO heads the root, so its answer is the whole company;
//! * a head with no filter is narrowed to its own subtree, not widened;
//! * a head naming a SIBLING's unit is refused `caller-out-of-scope`;
//! * a `Service` is admitted, unfenced;
//! * with no caller at all the route is `401 caller-unauthenticated` and
//!   discloses nothing.

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

const SLUG: &str = "lifecycle-fence";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/lifecycle-fence";

/// The COMPOSITE document key the handler compares against — a display slug
/// fails the label match, and a harness that used one would agree with itself
/// and disagree with the daemon.
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

async fn read(app: &axum::Router, scope: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({ "slug": wire_key() });
    if let Some(scope) = scope {
        body["scopeDepartmentId"] = Value::String(scope.to_owned());
    }
    let request = Request::builder()
        .method("POST")
        .uri("/v1/org/lifecycle-status/read")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// The unit ids in the answer, in order.
fn department_ids(body: &Value) -> Vec<String> {
    body.get("departments")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn person_ids(body: &Value) -> Vec<String> {
    body.get("people")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("personId").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// THE POSITIVE. The CEO heads the root department, so a CEO-scoped read
/// legitimately covers the whole company — that is the model, not an
/// exemption, and it is what stops the refusals below from being satisfied by
/// a route that refuses everybody.
#[tokio::test]
async fn the_ceo_sees_the_whole_company_with_no_filter() {
    let (app, _dir) = app(Some(person("chief"))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let mut ids = department_ids(&body);
    ids.sort();
    assert_eq!(ids, vec!["executive", "it", "quant"], "body: {body}");
    assert_eq!(person_ids(&body).len(), 4, "body: {body}");
}

/// AN OMITTED FILTER NARROWS, IT NO LONGER WIDENS. This is the whole defect:
/// before the fence, this exact request answered `quant-head` with every
/// department and every person in the company.
#[tokio::test]
async fn a_head_with_no_filter_is_narrowed_to_its_own_subtree() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body), vec!["quant"], "body: {body}");

    let mut people = person_ids(&body);
    people.sort();
    assert_eq!(people, vec!["quant-head", "signal-researcher"], "body: {body}");
}

/// A worker heads nothing, so the MUTATION predicate answers "no unit" for
/// them. The disclosure fence is deliberately the looser question — the unit
/// you live in — because a read fence that hid a worker's own department from
/// them would be a wrong gate rather than a missing one.
#[tokio::test]
async fn a_worker_sees_the_department_it_lives_in_and_nothing_beside_it() {
    let (app, _dir) = app(Some(person("signal-researcher"))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body), vec!["quant"], "body: {body}");
    assert!(!person_ids(&body).contains(&"it-head".to_owned()), "body: {body}");
}

/// THE REFUSAL. `quant-head` heads `quant`; `it` is a sibling, and nothing
/// reaches sideways.
#[tokio::test]
async fn a_head_naming_a_siblings_department_is_refused() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = read(&app, Some("it")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );

    // THE REFUSAL NAMES THE SUBTREE, NEVER A JOB TITLE (2026-08-13 ruling).
    let detail = body.get("detail").and_then(Value::as_str).unwrap_or_default();
    assert!(detail.contains("quant"), "the refusal must name the subtree: {detail}");
    for title in ["manager", "head-level", "CEO-level", "executive-only"] {
        assert!(!detail.contains(title), "a refusal must never name a job title: {detail}");
    }
}

/// The root department is UPWARD from a head, and upward is the one direction
/// the tree forbids. Asserted separately from the sibling case because a fence
/// that only compared ids would let this one through.
#[tokio::test]
async fn a_head_naming_the_root_department_is_refused() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = read(&app, Some("executive")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );
}

/// A head may still narrow INSIDE its own fence. A fence that refused every
/// supplied filter would pass the two tests above and break the caller that
/// legitimately asks a narrower question.
#[tokio::test]
async fn a_head_may_narrow_to_a_unit_inside_its_own_fence() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) = read(&app, Some("quant")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body), vec!["quant"], "body: {body}");
}

/// THE TRAP THIS PACKET IS BUILT AROUND. The resident actuator authenticates
/// as a SERVICE and every one of its HTTP calls is a read. `authenticated_person_id`
/// answers `None` for a service, so a fence built on it would authenticate the
/// actuator and then refuse it. A service names no person row, is never
/// resolved to one, and is admitted.
#[tokio::test]
async fn a_service_identity_is_admitted_and_sees_the_whole_company() {
    let (app, _dir) = app(Some(identity("chiefd-actuator", IdentityKind::Service))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body).len(), 3, "body: {body}");
    assert_eq!(person_ids(&body).len(), 4, "body: {body}");
}

/// The operator has no manifest entry and full scope by construction, so it is
/// admitted on the same rule as the service.
#[tokio::test]
async fn an_operator_identity_is_admitted_and_sees_the_whole_company() {
    let (app, _dir) = app(Some(identity("operator", IdentityKind::Operator))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body).len(), 3, "body: {body}");
}

/// A CHANNEL naming nobody is the third daemon-scoped kind, and it is asserted
/// here because it is the one both this packet and B2's `caller_person_to_authorize`
/// nearly left untested. The rule is "does the principal name a person row",
/// so a channel whose principal names one IS fenced and a channel that names
/// nobody is not — which is the correct behaviour for an attested pi-pane
/// either way, and falls out of the rule rather than being a case in it.
#[tokio::test]
async fn a_channel_identity_naming_nobody_is_admitted() {
    let (app, _dir) = app(Some(identity("pi-pane", IdentityKind::Channel))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body).len(), 3, "body: {body}");
}

/// And the other half of that rule, which is the reason the fence keys on the
/// PRINCIPAL rather than on the KIND: an attested pane channel that names a
/// real person is fenced exactly as that person, so attesting a channel does
/// not become a way to widen a head into the whole company.
#[tokio::test]
async fn a_channel_identity_naming_a_person_is_fenced_as_that_person() {
    let (app, _dir) = app(Some(identity("quant-head", IdentityKind::Channel))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(department_ids(&body), vec!["quant"], "body: {body}");
}

/// The one case where a missing person row must NOT be read as a missing
/// fence: a `Person`-kind credential the manifest does not have is stale or
/// foreign, and is refused rather than admitted unfenced.
#[tokio::test]
async fn a_person_credential_the_manifest_does_not_have_is_refused() {
    let (app, _dir) = app(Some(person("stranger"))).await;
    let (status, body) = read(&app, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert_eq!(
        body.get("code").and_then(Value::as_str),
        Some("caller-out-of-scope"),
        "body: {body}"
    );
}

/// NO CALLER, NOTHING DISCLOSED — in both request shapes, the unnarrowed read
/// and the one that names a department. The rollout arm that made absence mean
/// "local trust" is deleted, so neither shape can reach the projection, and
/// neither answer carries a department id.
#[tokio::test]
async fn without_a_caller_the_route_is_401() {
    let (app, _dir) = app(None).await;
    for target in [None, Some("it")] {
        let (status, body) = read(&app, target).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{target:?} body: {body}");
        assert_eq!(
            body.get("code").and_then(Value::as_str),
            Some("caller-unauthenticated"),
            "{target:?} body: {body}"
        );
        assert!(department_ids(&body).is_empty(), "{target:?} body: {body}");
    }
}
