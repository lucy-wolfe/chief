//! B2 increment 2: the `runtime_routes.rs` family reads its caller.
//!
//! Five mutating routes move the WHOLE company's runtime and asked nothing
//! about who was asking: `runtime/{launch,resume,stop}` and
//! `runtime/ownership/{claim,release}`. There were nine: chief-home-is-cwd §4c
//! deleted `runtime/launch-ceo-only` with the daemon-side CEO boot, and
//! 2026-08-24 deleted the three `company-session-action/*` routes with
//! `org_maintain_session`.
//!
//! None of them names a person target. `requestedPersonIds` on a launch WIDENS
//! the fleet rather than being the subject of the request, and a company session
//! action is company-wide by construction. So the department each one reaches is
//! the ROOT department, and every one takes the same whole-company fence the
//! `router.rs` half of B2 introduced — `department_is_in_scope` over the root,
//! never a job title.
//!
//! # Reading the positive cases
//!
//! Five of the eight need a runtime HOST, which no test router has, so the honest
//! positive for those is that an authorized caller reaches the capability check
//! and an unauthorized one never does. That is asserted directly: the CEO's
//! request is never `403 caller-out-of-company-scope`, and the head's always is.
//! The fence runs BEFORE the host check on purpose — a caller with no authority
//! must not be able to tell, from a 503 versus a 403, whether the daemon it is
//! talking to holds a runtime host.
//!
//! The remaining three need no host and give a full `200` positive.

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

const SLUG: &str = "b2-runtime-family";
/// The company's own directory. A company IS a directory, and its identity
/// is the hash of that path — nothing here is a name.
const COMPANY_DIR: &str = "/data/orgs/b2-runtime-family";

/// The COMPOSITE document key the handlers compare against — a display slug
/// fails the own-company match.
fn wire_key() -> String {
    host_primitives::rendezvous::company_key(std::path::Path::new(COMPANY_DIR))
}

fn identity(kind: IdentityKind, principal: &str, company: Option<String>) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id-{principal}"),
        principal: principal.to_owned(),
        kind,
        company_slug: company,
        pubkey: Some("test-key".to_owned()),
        fingerprint: format!("fp-{principal}"),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

fn person(principal: &str) -> CallerIdentity {
    identity(IdentityKind::Person, principal, Some(wire_key()))
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
        Some(caller) => router.layer(Extension(caller)),
        None => router,
    };
    (router, dir)
}

async fn post(app: &axum::Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn code(body: &Value) -> Option<&str> {
    body.get("code").and_then(Value::as_str)
}

/// Every fenced route in the family, with a body its handler accepts.
fn every_fenced_route() -> Vec<(&'static str, Value)> {
    let launch = json!({
        "slug": wire_key(),
        "requestedPersonIds": ["signal-researcher"],
        "actor": "operator"
    });
    vec![
        ("/v1/org/runtime/launch", launch.clone()),
        ("/v1/org/runtime/resume", launch),
        ("/v1/org/runtime/stop", json!({ "slug": wire_key() })),
        ("/v1/org/runtime/ownership/claim", json!({ "slug": wire_key() })),
        ("/v1/org/runtime/ownership/release", json!({ "slug": wire_key() })),
        // TOMBSTONE: the three `company-session-action` routes that were fenced
        // here — `queue`, `skip-parked` and `reconcile-claims`. Deleted with
        // `org_maintain_session`; nothing in production could queue one. The
        // FENCE they were fenced by is unchanged and still covered by the five
        // runtime routes above, which take the identical whole-company check.
    ]
}

/// THE REFUSAL, ONE PER ROUTE, with a stable code. `quant-head` heads `quant`
/// and never the root, so it may not move the whole company's runtime.
#[tokio::test]
async fn a_head_may_not_move_the_whole_companys_runtime() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    for (path, body) in every_fenced_route() {
        let (status, out) = post(&app, path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {out}");
        assert_eq!(code(&out), Some("caller-out-of-company-scope"), "{path}: {out}");
    }
}

/// THE POSITIVE CASE. The CEO heads the root, so no route in the family refuses
/// it on authority. Five of the eight then meet the host-capability check this
/// router cannot satisfy, which is a different answer entirely and is exactly
/// what proves the fence discriminates rather than refusing everybody.
#[tokio::test]
async fn the_ceo_is_never_refused_on_authority() {
    let (app, _dir) = app(Some(person("chief"))).await;
    for (path, body) in every_fenced_route() {
        let (status, out) = post(&app, path, body).await;
        assert_ne!(
            code(&out),
            Some("caller-out-of-company-scope"),
            "the CEO heads the root and must never be refused on authority: {path}: {out}"
        );
        assert_ne!(status, StatusCode::FORBIDDEN, "{path}: {out}");
    }
}

// TOMBSTONE: `the_ceo_drives_the_hostless_company_actions_to_completion` was
// here. It anchored the positive above with a full `200` from the two routes in
// this family that needed no runtime host — `company-session-action/skip-parked`
// and `.../reconcile-claims`. Both are deleted, and every SURVIVING route in the
// family reaches a real host, so no request in this family can complete inside a
// test router any more. The positive above is therefore weaker by exactly that
// much: it proves the CEO is not refused ON AUTHORITY, not that the work runs.
// Restore an equivalent the day a hostless route joins this family.

/// A worker heads nothing, which is the case that shows the fence is the
/// SUBTREE and not a title — refused for the same reason a head is, one level
/// further out.
#[tokio::test]
async fn a_worker_may_not_move_the_whole_companys_runtime() {
    let (app, _dir) = app(Some(person("signal-researcher"))).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/ownership/release", json!({ "slug": wire_key() })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-out-of-company-scope"), "body: {body}");
}

/// A person identity is company-scoped, and this is refused before any scope
/// question is asked.
#[tokio::test]
async fn a_person_of_another_company_is_refused_first() {
    let foreign = identity(IdentityKind::Person, "chief", Some("northstar@/data/orgs".to_owned()));
    let (app, _dir) = app(Some(foreign)).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/ownership/release", json!({ "slug": wire_key() })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(code(&body), Some("caller-company-mismatch"), "body: {body}");
}

/// THE FENCE KEYS ON THE PRINCIPAL, NOT ON THE KIND. `pi-pane` is a `channel`
/// identity for A PERSON's pane, and `identities`' schema comment says two
/// identities may share one principal. A gate that admitted on kind would make
/// getting a channel attested a way to launch or stop a whole company without
/// heading anything, so a credential naming a person row is fenced AS that
/// person whatever its kind — including on the launch path.
#[tokio::test]
async fn a_channel_attested_as_a_person_may_not_launch_the_company() {
    for kind in [IdentityKind::Channel, IdentityKind::Service, IdentityKind::Operator] {
        let (app, _dir) = app(Some(identity(kind, "quant-head", None))).await;
        for (path, body) in every_fenced_route() {
            let (status, out) = post(&app, path, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{kind:?} {path}: {out}");
            assert_eq!(code(&out), Some("caller-out-of-company-scope"), "{kind:?} {path}: {out}");
        }
    }
}

/// THE OPERATOR CLIENT KEEPS LAUNCHING. A daemon-scoped identity whose principal
/// names NO person row holds the unconditional scope `control_authority` grants
/// `ControlActor::Operator`.
///
/// It asserted `200` while this family still had a hostless member. Both of
/// those are deleted, so every surviving route reaches a real host that no test
/// router has, and the honest positive is the one the module doc names: the
/// caller gets PAST the fence and dies on the missing capability. That is a
/// stronger statement than "not 403" — `no-runtime-host-capability` is decided
/// after the scope check, so only a caller that passed the fence can see it.
#[tokio::test]
async fn a_daemon_scoped_identity_keeps_its_unconditional_scope() {
    for kind in [IdentityKind::Operator, IdentityKind::Service, IdentityKind::Channel] {
        let (app, _dir) = app(Some(identity(kind, "operator", None))).await;
        let (status, body) =
            post(&app, "/v1/org/runtime/ownership/release", json!({ "slug": wire_key() })).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{kind:?}: {body}");
        assert_eq!(code(&body), Some("no-runtime-host-capability"), "{kind:?}: {body}");
    }
}

/// NO CALLER, NO ROUTE — on EVERY member of the family, which is why this
/// iterates `every_fenced_route()` rather than sampling one. The rollout arm
/// that made absence mean "local trust" is deleted, so the refusal is `401
/// caller-unauthenticated` and not the `403 caller-out-of-company-scope` a real
/// principal in the wrong company gets: the two send the caller to different
/// fixes.
#[tokio::test]
async fn without_a_caller_every_route_in_the_family_is_401() {
    let (app, _dir) = app(None).await;
    for (path, body) in every_fenced_route() {
        let (status, out) = post(&app, path, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {out}");
        assert_eq!(code(&out), Some("caller-unauthenticated"), "{path}: {out}");
    }
}

/// The fence runs after the own-company filter, so a foreign slug is still the
/// 404 it always was rather than a 403 that would leak whether it exists.
#[tokio::test]
async fn a_foreign_slug_is_still_unknown_company() {
    let (app, _dir) = app(Some(person("quant-head"))).await;
    let (status, body) =
        post(&app, "/v1/org/runtime/ownership/release", json!({ "slug": "foreign@company" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(code(&body), Some("unknown-company"), "body: {body}");
}
