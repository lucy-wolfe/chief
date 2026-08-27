//! The three reminder fixtures of the `tools` family, replayed against the
//! REAL `/v1/reminders/*` routes.
//!
//! # Why this file exists
//!
//! `conformance_tools.rs` holds 137 `tools` fixtures in a named quarantine
//! because they project `packages/piing/extensions/organization-intercom.ts`
//! and there is no Rust tool registry to replay them against. That is true of
//! the tool *host* — registration, schemas, card rendering — and it was quietly
//! read as though it were true of everything a tool fixture records.
//!
//! It is not. `org_create_reminder`, `org_list_reminders` and
//! `org_stop_reminder` stopped shelling out to `apps/cli` and now `postOrgRoute`
//! to `/v1/reminders/arm|list|stop` (`organization-intercom.ts:12558`). Those
//! routes ARE Rust, in this crate, and their answer IS the `details` payload the
//! tool hands back — the three tools pass chiefd's response through untouched.
//! So the half of each fixture that says *what the company did* has a Rust
//! subject today, and `conformance/README.md`'s retirement condition ("the
//! harness retires when the `tools` family has a Rust subject") is met for
//! exactly these three.
//!
//! # What the three fixtures used to assert
//!
//! An argv transport that no longer exists —
//! `["org","reminder","arm","northstar-conformance"]` through
//! `tools.launcher_calls` — and a `createdByPersonId` field in the REQUEST
//! payload, which `ArmReminderRequest` does not have and which DECISIONS.md
//! (2026-08-10) took off the wire when caller identity moved onto enrolled keys.
//! Both survived because nothing executed them: the quarantine shape-checks, and
//! the TypeScript runner has not parsed since merge `b887b9a9c`.
//!
//! `createdByPersonId` on the RESPONSE record is a different fact and is still
//! true; `reminders_http_surface.rs` pins it, and these fixtures keep it.
//!
//! # What this file replays
//!
//! For each fixture: seed a fresh northstar company, then POST every call
//! recorded in its `tools.chiefd_calls` expectation, in order, as the fixture's
//! own caller — and assert the LAST response body equals the fixture's
//! `expect.ok.details` byte for byte. The clock is a `ManualClock` parked on the
//! conformance epoch, so `createdAt` and `nextDueAt` are the fixture's own
//! literals rather than wall time.
//!
//! # What it deliberately does not replay
//!
//! The tool's own half — argument canonicalization, the `@` strip, the card, and
//! the exact `message` string — is TypeScript and stays TypeScript (bucket A/B).
//! `expect.ok.message` is therefore checked only for the facts it must carry
//! from the record it describes: the real reminder id, and the real `nextDueAt`
//! or `fireCount`. That is what catches a message quoting a fabricated
//! `reminder-conformance` id, which is what these fixtures did.
//! `packages/piing/test/toolcontract/OrganizationToolContract.test.ts` drives
//! the tool end live.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::authn::middleware::CallerIdentity;
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::store::identities::{Identity, IdentityKind};
use chiefd_core::store::{organization, supervision};
use chiefd_core::test_support::{northstar_manifest, ManualClock};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// The frozen instant every fixture is recorded against, read from its ONE
/// definition rather than restated. It used to be a private copy here citing
/// `conformance/lib/world.ts`, which is deleted; four such copies had to agree
/// and nothing compared them.
use chiefd_core::test_support::CONFORMANCE_EPOCH as EPOCH;

/// The three fixtures this file is the Rust subject for.
///
/// Named, not globbed: `conformance_tools.rs` reads this same list out of this
/// file's source to prove its own "replayed elsewhere" set still matches a real
/// replay. A glob would let a fixture silently leave one side.
const REPLAYED: &[&str] = &[
    "org-create-reminder-arms-a-durable-recurring-reminder-for-yourself",
    "org-list-reminders-lists-your-own-durable-reminders",
    "org-stop-reminder-disarms-one-reminder-and-retains-the-row",
];

/// The repo root, four levels above this crate. Same #1002 staleness check as
/// `conformance_common::repo_root` — a shared `CARGO_TARGET_DIR` can serve a
/// binary built from a checkout that has since been deleted, and a bare "file
/// not found" would send the reader looking for a missing fixture.
fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        manifest.is_dir(),
        "this test binary was compiled with CARGO_MANIFEST_DIR={} baked in, but that directory \
         no longer exists on this host (#1002: a shared CARGO_TARGET_DIR served a binary built \
         from a deleted checkout). Fix: `cargo clean -p chiefd-api` and rebuild.",
        manifest.display()
    );
    manifest
        .ancestors()
        .nth(4)
        .expect("the repo root is four levels above this crate")
        .to_path_buf()
}

fn fixture(name: &str) -> Value {
    let path = repo_root().join("conformance/fixtures/tools").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

struct World {
    _dir: tempfile::TempDir,
    path: PathBuf,
    company: Arc<CompanyDb>,
    slug: String,
}

async fn seeded_world(tag: &str) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let slug = format!("northstar-conformance@{tag}");
    let manifest = northstar_manifest(EPOCH);
    // Parked, never advanced: `arm_reminder` stamps `createdAt` from this clock
    // and derives `nextDueAt` from it, and both are fixture literals.
    let clock = Arc::new(ManualClock::starting_at(0, EPOCH));
    let company =
        Arc::new(CompanyDb::open("northstar-conformance", &path, clock).expect("open company db"));
    company
        .mutate(MutationClass::Normal, MutationName("company.create"), {
            let seed = manifest.clone();
            move |ledgers| {
                organization::create(ledgers, &seed)?;
                supervision::seed(ledgers, &seed)?;
                Ok(())
            }
        })
        .await
        .expect("company creation commits");
    World { _dir: dir, path, company, slug }
}

async fn app_for(world: &World) -> axum::Router {
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&world.path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&world.company), world.slug.clone());
    router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
}

/// The identity the auth layer would have resolved from a verified bearer token.
/// The reminder routes read the CREATOR off this, which is the whole reason
/// `createdByPersonId` is not in the request body.
fn caller(principal: &str) -> CallerIdentity {
    CallerIdentity(Identity {
        identity_id: format!("id:{principal}"),
        principal: principal.to_string(),
        kind: IdentityKind::Person,
        company_slug: Some("northstar-conformance".to_string()),
        pubkey: Some("spki".to_string()),
        fingerprint: "fp".to_string(),
        active: true,
        enrolled_at: EPOCH,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn post_as(app: &axum::Router, actor: &str, uri: &str, body: &Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    request.extensions_mut().insert(caller(actor));
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{uri} answered non-JSON ({e}): {text}"));
    (status, parsed)
}

/// The `tools.chiefd_calls` expectation: the ordered list of `{path, body}` the
/// tool posted. This is the fixture's transport claim, and replaying it is what
/// turns that claim into something the product can refute.
fn recorded_calls(fixture: &Value, name: &str) -> Vec<(String, Value)> {
    let states = fixture["expectState"].as_array().expect("`expectState` is an array");
    let state = states
        .iter()
        .find(|state| state["read"] == "tools.chiefd_calls")
        .unwrap_or_else(|| panic!("{name}: no `tools.chiefd_calls` expectation to replay"));
    state["equals"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: `equals` is not an array"))
        .iter()
        .map(|call| {
            let path = call["path"].as_str().expect("a route path").to_string();
            (path, call["body"].clone())
        })
        .collect()
}

/// Replay one fixture and return the last route answer.
///
/// The recorded `slug` is the redacted `northstar-conformance@<ROOT_DIGEST>`
/// form the harness writes (a company key carries a root digest that is
/// deliberately not a fixture value), so this substitutes the live world's slug.
/// Nothing else in a recorded body is touched — the point is to send exactly
/// what the tool sends.
async fn replay(name: &str) -> (Value, Value) {
    let fixture = fixture(name);
    let world = seeded_world(name).await;
    let app = app_for(&world).await;
    let actor = fixture["caller"]["personId"].as_str().expect("a caller personId");

    let calls = recorded_calls(&fixture, name);
    assert!(!calls.is_empty(), "{name}: a replay needs at least one recorded call");
    let mut last = Value::Null;
    for (path, body) in calls {
        let mut body = body;
        assert!(
            body.get("createdByPersonId").is_none(),
            "{name}: `createdByPersonId` is not on the request wire — the creator is the \
             authenticated caller (DECISIONS.md 2026-08-10). A body carrying it is a caller \
             telling the daemon who it is.",
        );
        body["slug"] = Value::String(world.slug.clone());
        let (status, answer) = post_as(&app, actor, &path, &body).await;
        assert_eq!(status, StatusCode::OK, "{name}: {path} refused the recorded body: {answer}",);
        last = answer;
    }
    (fixture, last)
}

/// The recorded response IS the tool's `details`, byte for byte.
///
/// The three reminder tools pass chiefd's answer straight into `toolResult`'s
/// details, so this is not an approximation of the fixture — it is the same
/// value, and any drift in the served record fails here with a diff.
async fn assert_details_match(name: &str) -> String {
    let (fixture, answer) = replay(name).await;
    let expected = &fixture["expect"]["ok"]["details"];
    assert_eq!(
        &answer,
        expected,
        "\n{name}: the served record no longer equals the fixture.\n  served:   {}\n  fixture:  {}\n",
        serde_json::to_string_pretty(&answer).unwrap_or_default(),
        serde_json::to_string_pretty(expected).unwrap_or_default(),
    );
    fixture["expect"]["ok"]["message"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_create_reminder_arms_over_the_reminders_route() {
    let name = REPLAYED[0];
    let message = assert_details_match(name).await;
    let fixture = fixture(name);
    let reminder = &fixture["expect"]["ok"]["details"]["reminder"];
    // The message must quote the record it describes. The fixture this replaces
    // announced `reminder-conformance`, an id chiefd has never minted.
    for fact in ["id", "nextDueAt"] {
        let value = reminder[fact].as_str().expect("a string fact");
        assert!(
            message.contains(value),
            "{name}: the message does not quote the served {fact} '{value}': {message}",
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_list_reminders_lists_over_the_reminders_route() {
    let name = REPLAYED[1];
    assert_details_match(name).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_stop_reminder_stops_over_the_reminders_route() {
    let name = REPLAYED[2];
    let message = assert_details_match(name).await;
    let fixture = fixture(name);
    let reminder = &fixture["expect"]["ok"]["details"]["reminder"];
    let id = reminder["id"].as_str().expect("an id");
    assert!(message.contains(id), "{name}: the message does not quote the served id '{id}'");
    assert_eq!(reminder["status"], "stopped", "{name}: stop disarms rather than deletes");
}

/// Every replayed fixture names the route family it is the subject of.
///
/// The failure this catches is the one that produced this file: a fixture whose
/// `expectState` names a transport the product does not have. A `tools.*`
/// launcher read here would mean the fixture drifted back onto argv.
#[test]
fn every_replayed_fixture_records_the_http_seam_and_nothing_else() {
    for name in REPLAYED {
        let fixture = fixture(name);
        let reads: Vec<String> = fixture["expectState"]
            .as_array()
            .expect("`expectState` is an array")
            .iter()
            .map(|state| state["read"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reads,
            vec!["tools.chiefd_calls".to_string()],
            "{name}: a replayed reminder fixture records the HTTP seam only, but reads {reads:?}",
        );
        for (path, _) in recorded_calls(&fixture, name) {
            assert!(path.starts_with("/v1/reminders/"), "{name}: '{path}' is not a reminder route",);
        }
    }
}
