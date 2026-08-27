//! The supervision-row HTTP surface — `/v1/org/supervision/read`
//! (org-data-normalization P0, N3).
//!
//! ═══ What this file is FOR ═══
//!
//! `CompanyDb::supervision_read/publish` reconstruct and diff the WHOLE
//! `SupervisionLedger` over the normalized rows, but the READ engine is
//! unreachable until the route exists — the same "engine tested, feature
//! reachable" gap the reminders surface paid for. This file proves the read
//! route end to end over REAL HTTP: a written ledger reconstructs, the
//! `ifSeqNot` fast path omits the body, the relational half survives, and a
//! foreign slug is isolated.
//!
//! ═══ Why the ledger is WRITTEN in-process ═══
//!
//! `/v1/org/supervision/{publish,publish-cas,clear}` are deleted. The
//! publisher-route sweep found no caller of any kind: the TypeScript
//! `RowSupervisionRepository` (`org-durable-store.ts`) that once posted them
//! no longer exists, and nothing replaced it. The route handler was a
//! one-line pass-through to `company.supervision_publish(raw_body)`, so
//! calling that method directly exercises the IDENTICAL write path —
//! including the relational-half re-adoption from the raw body, which is the
//! subtle part these tests exist for. What is lost is the HTTP door, and
//! nobody was standing at it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chiefd_api::docstore::{
    router_with_supervision_live, ChangeFeed, DocStore, SupervisionLiveSource,
};
use chiefd_core::actor::{CompanyDb, MutationClass, MutationName};
use chiefd_core::clock::SystemClock;
use chiefd_core::store::organization;
use chiefd_core::test_support::northstar_manifest;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct World {
    _dir: tempfile::TempDir,
    company: Arc<CompanyDb>,
    slug: String,
    app: axum::Router,
}

/// A company whose supervision ROWS start empty (no `supervision_meta` row yet):
/// the row port publishes into a blank slate, exactly the strangler cut-over.
async fn world(tag: &str) -> World {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(chiefd_core::store::COMPANY_DB_FILENAME);
    let slug = format!("cobalt@{tag}");
    // label == slug so the internal row_slug (`self.label()`) and the route's
    // own-company gate (`org_documents_slug`) name the same key.
    let company = Arc::new(
        CompanyDb::open(&slug, &path, Arc::new(SystemClock::default())).expect("open company db"),
    );
    // `supervision_publish` reads the manifest to validate the incoming ledger
    // (unknown-company 422 without one), and `supervision::validate` requires
    // the ledger's `organization` to equal the manifest slug and every manager
    // id to name a real non-worker person. Create ONLY the manifest — not the
    // full genesis, whose supervision/activity seeds would break this file's
    // blank-slate premise ("no supervision rows yet"). The sample ledger's
    // manager "m1" is added as a cloned head for the same reason.
    let mut manifest = northstar_manifest(1_784_116_800_000);
    manifest.slug = slug.clone();
    let mut manager = manifest.people["quant-head"].clone();
    manager.id = "m1".to_string();
    manager.name = "Manager One".to_string();
    // A leader must head exactly one department (D3): give m1 its own unit
    // under the root rather than leaving it heading nothing.
    manager.department_id = "mgmt".to_string();
    manifest.people_order.push(manager.id.clone());
    manifest.people.insert(manager.id.clone(), manager);
    let mut unit = manifest.departments["it"].clone();
    unit.id = "mgmt".to_string();
    unit.name = "Management".to_string();
    unit.head_person_id = "m1".to_string();
    manifest.department_order.push(unit.id.clone());
    manifest.departments.insert(unit.id.clone(), unit);
    // The wire-authored-effects test's recipient "w1" must also be a real
    // manifest person (validate refuses unknown ownership).
    let mut worker = manifest.people["signal-researcher"].clone();
    worker.id = "w1".to_string();
    worker.name = "Worker One".to_string();
    manifest.people_order.push(worker.id.clone());
    manifest.people.insert(worker.id.clone(), worker);
    company
        .mutate(MutationClass::Normal, MutationName("company.create"), move |ledgers| {
            organization::create(ledgers, &manifest).map(|_| ())
        })
        .await
        .expect("manifest creation commits");
    let feed = Arc::new(ChangeFeed::new());
    let store = Arc::new(
        DocStore::open_with_feed(&path.display().to_string(), 2, Arc::clone(&feed))
            .expect("open docstore"),
    );
    store.ensure_schema().await.expect("schema");
    let live = SupervisionLiveSource::new(Arc::clone(&company), slug.clone());
    let app =
        router_with_supervision_live(store, 256 * 1024 * 1024, Duration::from_secs(15), Some(live))
            .layer(axum::extract::Extension(ceo_caller(&slug)));
    World { _dir: dir, company, slug, app }
}

/// The credential every request in this file carries: northstar's CEO, as a
/// PERSON, scoped by the COMPOSITE document key the own-company gate keys on.
///
/// The CEO because these are row-surface contract tests and the CEO heads the
/// root, so no authority refusal can stand in front of the wire behaviour under
/// test.
///
/// It is not optional: with the absent-caller arm deleted, a request with no
/// identity never reaches the handler.
fn ceo_caller(company_slug: &str) -> chiefd_api::authn::middleware::CallerIdentity {
    chiefd_api::authn::middleware::CallerIdentity(chiefd_core::store::identities::Identity {
        identity_id: "id-ceo".to_owned(),
        principal: "chief".to_owned(),
        kind: chiefd_core::store::identities::IdentityKind::Person,
        company_slug: Some(company_slug.to_owned()),
        pubkey: Some("test-key".to_owned()),
        fingerprint: "fp-ceo".to_owned(),
        active: true,
        enrolled_at: 0,
        enrolled_by: None,
        revoked_at: None,
    })
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, String) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("response is not JSON ({e}): {body}"))
}

/// A minimal, internally-valid ledger for `slug` with one armed reminder, so
/// the round-trip proves an ENTITY row (not just the meta row) survives.
fn sample_ledger(slug: &str, extra: Option<(&str, Value)>) -> Value {
    let mut reminder = json!({
        "id": "r1",
        "personId": "m1",
        "createdByPersonId": "m1",
        "prompt": "hold the risk limits",
        "intervalMs": 900000,
        "nextDueAt": "2026-07-25T00:00:00Z",
        "status": "active",
        "recurring": true,
        "fireCount": 0,
        "createdAt": "2026-07-25T00:00:00Z",
    });
    if let Some((k, v)) = extra {
        reminder.as_object_mut().unwrap().insert(k.to_string(), v);
    }
    json!({
        "schemaVersion": 2,
        "organization": slug,
        "reminderOrder": ["r1"],
        "reminders": { "r1": reminder },
        "createdAt": "2026-07-25T00:00:00Z",
        "updatedAt": "2026-07-25T00:00:00Z",
    })
}

/// Write a ledger the way production writes it: through `CompanyDb`, from the
/// RAW body, so the relational-half adoption under test really runs.
async fn write_ledger(w: &World, ledger: &Value) -> i64 {
    w.company.supervision_publish(ledger.to_string()).await.expect("the ledger commits")
}

/// The machine code a refused write answers with.
async fn refusal_code(w: &World, ledger: &Value) -> String {
    match w.company.supervision_publish(ledger.to_string()).await {
        Ok(seq) => panic!("this ledger must be refused, but it committed at seq {seq}"),
        Err(chiefd_core::error::ChiefdError::Refused(refusal)) => refusal.code.to_string(),
        Err(other) => panic!("expected a validation refusal, got {other:?}"),
    }
}

// TOMBSTONE (chief-home-is-cwd §4c):
// `ceo_only_prepare_clears_every_report_fence_asks_for_the_root_and_quiesces_delivery`
// stood here. It drove `POST /v1/org/runtime/prepare-ceo-only` and asserted the
// committed answer, the fence collapsing to the root alone, and the quiesce
// watermark advancing. The route is deleted with the daemon-side CEO boot. The
// STORE operation it exercised is unchanged and is still pinned, by
// `ceo_only_prepare_retracts_everybody_else_and_asks_for_the_root_by_name` and
// its four neighbours in `chiefd-core/src/store/org_ops.rs` — genesis is the
// caller now.

// TOMBSTONE: `prepare_ceo_only_reports_prepared_once_somebody_is_actuating`.
//
// It was the other half of the pair above: with an actuation lease actually
// held, the SAME call reported `prepared: true` and `presence: "present"`. Both
// halves rested on the ONE way an actuator could attach -- POSTing an
// observation to `/v1/org/runtime/observed` -- and that route, that lease and
// that verdict are all deleted with the upward direction they belonged to.
//
// It cannot be re-based, because there is no longer any way for a test (or a
// real actuator) to make chiefd believe somebody is actuating: that is the
// point of the change, not a gap in it. The surviving half above asserts what
// chiefd still answers AND that neither deleted claim can come back.

// TOMBSTONE: the five `*_rejects_the_retired_expected_seq_payload_field` tests
// (activity, mailbox, session-maintenance, supervision) and
// `activity_structural_reconcile_is_data_free_and_scoped_to_the_live_company`.
// Each pinned that a DELETED publish route refuses a caller-supplied sequence.
// The rule they protect — no caller-side CAS on the direct atomic contract —
// now has exactly one route left that can break it, and
// `org_row_seam_b4.rs::the_direct_atomic_singleton_publish_rejects_a_retired_expected_seq_payload`
// pins it there.

/// #983: `session_maintenance_read`'s not-found branch used to hardcode
/// `seq: 0`, but the seq it reports is the COMPANY-WIDE `org_events` cursor --
/// the same cursor a supervision write (or any other mutation) also advances --
/// not a per-document fence starting at 0. A company with zero prior
/// activity cannot exercise the bug (0 happens to be correct there too),
/// which is exactly the gap that let it sit unexercised: this test
/// deliberately generates prior UNRELATED activity first, so the real
/// cursor is provably nonzero before the company's first-ever
/// session-maintenance read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_session_maintenance_read_reports_the_real_cursor_after_unrelated_activity() {
    let w = world("session-maintenance-first-read-after-activity").await;
    let ground_truth_seq = ground_truth_org_events_seq(&w).await;

    // The company has never published session-maintenance -- `found` must be
    // false, and (the fix under test) `seq` must equal the SAME real
    // cursor, not a hardcoded 0.
    let (status, body) =
        post(&w.app, "/v1/org/session-maintenance/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let read = parse(&body);
    assert_eq!(read["found"], false, "{body}");
    assert_eq!(
        read["seq"].as_i64().expect("session-maintenance read carries a seq"),
        ground_truth_seq,
        "session-maintenance/read's not-found seq must match the real org_events cursor, not a hardcoded 0: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_written_ledger_reconstructs_through_the_read_route() {
    let w = world("rowtrip").await;

    // Empty rows ⇒ read is found:false with the starting audit cursor.
    let (status, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = parse(&body);
    assert_eq!(r["found"], false, "no supervision rows yet");
    let before = r["seq"].as_i64().expect("seq");

    // Write the semantic ledger directly from current SQLite state.
    let new_seq = write_ledger(&w, &sample_ledger(&w.slug, None)).await;
    assert!(new_seq > before, "the audit cursor advances on a write");

    // Read it back: the reminder survives the reconstruct.
    let (status, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = parse(&body);
    assert_eq!(r["found"], true, "the written ledger is now present");
    assert_eq!(r["seq"], new_seq, "read observes the post-write audit cursor");
    let round: Value = serde_json::from_str(r["ledger"].as_str().expect("ledger")).unwrap();
    assert_eq!(round["reminders"]["r1"]["prompt"], "hold the risk limits");
    assert_eq!(round["reminders"]["r1"]["intervalMs"], 900000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_matching_audit_cursor_returns_unchanged_without_reconstructing_a_ledger_body() {
    let w = world("conditional").await;
    let seq = write_ledger(&w, &sample_ledger(&w.slug, None)).await;

    let (status, body) =
        post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug, "ifSeqNot": seq })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response = parse(&body);
    assert_eq!(response["found"], true);
    assert_eq!(response["seq"], seq);
    assert_eq!(response["unchanged"], true);
    assert!(
        response.get("ledger").is_none(),
        "the unchanged fast path must omit the aggregate body: {response}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_authored_effects_survive_the_read_route() {
    // THE CONTROL a pure in-memory test cannot provide: effects and
    // nextEffectSequence are #[serde(skip)] on SupervisionLedger, so they only
    // survive if the write path re-adopts them from the RAW body and the read
    // route splices them back. Both halves are asserted here.
    let w = world("wirehalf").await;
    let mut ledger = sample_ledger(&w.slug, None);
    let obj = ledger.as_object_mut().unwrap();
    obj.insert("effectOrder".into(), json!(["e1"]));
    obj.insert(
        "effects".into(),
        json!({ "e1": {
            "id": "e1", "sequence": 1, "type": "person_reminder", "status": "pending",
            "createdAt": "2026-07-25T00:00:00Z", "reminderId": "r1"
        }}),
    );
    obj.insert("nextEffectSequence".into(), json!(2));
    write_ledger(&w, &ledger).await;

    // PROBE 1: did the WRITE persist the rows? Read the company in-process
    // (typed accessors bypass any response-serialization skip).
    let (typed, _seq) = w.company.supervision_read().await.expect("read").expect("present");
    assert_eq!(typed.effect_order(), ["e1"], "WRITE side: effect row persisted");
    assert_eq!(typed.next_effect_sequence(), 2, "WRITE side: counter persisted");

    let (status, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r = parse(&body);
    assert_eq!(r["found"], true, "{body}");
    let round: Value = serde_json::from_str(r["ledger"].as_str().expect("ledger")).unwrap();
    assert_eq!(round["effectOrder"], json!(["e1"]), "wire effect survived the HTTP route: {round}");
    assert_eq!(
        round["nextEffectSequence"],
        json!(2),
        "counter advanced (else 'effect sequence invalid')"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_semantic_write_uses_current_state_and_overwrites_the_prior_document() {
    let w = world("stale").await;

    // The first direct write establishes the current normalized rows.
    let first_seq = write_ledger(&w, &sample_ledger(&w.slug, None)).await;

    // A second writer supplies only a newer semantic document; it never
    // receives or retries a stale conditional write.
    let mut replacement = sample_ledger(&w.slug, None);
    replacement["reminders"]["r1"]["prompt"] = json!("revised risk limits");
    let second_seq = write_ledger(&w, &replacement).await;
    assert!(second_seq > first_seq, "the current-state write applies and advances the cursor");

    let (status, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let read = parse(&body);
    let ledger: Value = serde_json::from_str(read["ledger"].as_str().expect("ledger")).unwrap();
    assert_eq!(ledger["reminders"]["r1"]["prompt"], "revised risk limits");
}

/// Item D — the reject-extra fixture. An unmodeled, NON-legacy key must be
/// refused with `unmodeled-keys`, never a silent drop and never a success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_refuses_an_unmodeled_key() {
    let w = world("rejectextra").await;
    let code = refusal_code(&w, &sample_ledger(&w.slug, Some(("bogusField", json!("x"))))).await;
    assert_eq!(code, "unmodeled-keys", "the machine code");

    // And nothing was written — the row read still finds no ledger.
    let (_s, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(parse(&body)["found"], false, "a refused write writes nothing");
}

/// Item D both-halves: an ALLOWLISTED legacy key is REFUSED by the strict
/// write (write-STRICT), exactly like any other unmodeled key — the
/// read-tolerance that drops it lives in `reconstruct`, tested in the core rows
/// unit suite; a writer can never re-persist the legacy shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_refuses_an_allowlisted_legacy_key_too() {
    let w = world("legacywrite").await;
    // `cadenceMs` is on the READ allowlist (a pre-rename cadence key).
    let code = refusal_code(&w, &sample_ledger(&w.slug, Some(("cadenceMs", json!(60000))))).await;
    assert_eq!(code, "unmodeled-keys", "the write stays strict");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreign_slug_is_isolated() {
    let w = world("iso").await;

    // Read for a slug this process does not serve ⇒ found:false, never a leak.
    let (status, body) =
        post(&w.app, "/v1/org/supervision/read", json!({ "slug": "someone-else@zzz" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse(&body)["found"], false, "foreign read is isolated: {body}");

    // The own company is untouched.
    let _ = &w.company;
}

/// #984: `direct_org_row_route_pair!`'s shared not-found branch had the
/// IDENTICAL defect #983 fixed in `session_maintenance_read`/
/// `org_supervision_read` -- every route this macro family generates fences
/// its seq on the company-wide `org_events` cursor, not a per-document one
/// starting at 0. But whether that hardcoded `seq: 0` was actually REACHABLE
/// turned out to depend on a second, independent fact per type: whether
/// `$read_method`'s own Rust body can return a genuine `None` at all.
///
/// **`operator_escalation_intents_read`** unconditionally wraps
/// `Ok(Some((reconstruct(...)?, seq)))` -- `reconstruct` returns the bare
/// document type (an empty one when no rows exist), never `Option`. Its
/// generated route can NEVER observe the `None` arm at all: `found` is always
/// `true`, seq is always the real `current_seq`, and the hardcoded `0` in that
/// branch was already dead code for it specifically, discovered while writing
/// this test (the first attempt asserted `found: false` and failed on a
/// company with an empty-but-already-`found:true` document instead).
///
/// Same trap #983 named: a fresh-company fixture with zero prior activity
/// passes under both the broken and fixed code, so this generates real prior
/// activity first and cross-checks the true cursor through a route the bug
/// never touched (`/v1/org/supervision/read`, once a ledger exists) before
/// asserting the reads report the SAME cursor.
async fn ground_truth_org_events_seq(w: &World) -> i64 {
    // Prior UNRELATED activity: write a supervision ledger. This advances the
    // shared org_events cursor -- world()'s own company.create already
    // advances it once, but writing here gives an independent,
    // story-external assertion point (the supervision read below) to
    // cross-check the real cursor against.
    write_ledger(w, &sample_ledger(&w.slug, None)).await;
    let (status, body) = post(&w.app, "/v1/org/supervision/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let seq = parse(&body)["seq"].as_i64().expect("supervision read carries a seq");
    assert!(
        seq > 0,
        "the regression is only exercised if prior activity advanced the cursor past 0, got {seq}"
    );
    seq
}

/// `operator_escalation_intents_read` is unconditional
/// (`Ok(Some((reconstruct(...)?, seq)))`), so a company with zero prior
/// intents still reads `found: true` with an empty document -- discovered by
/// this test's first draft, which wrongly assumed a not-found state and
/// failed on `left: Bool(true), right: false`. There is no not-found
/// regression to demonstrate for it (the macro's `None` branch is
/// unreachable), so this asserts the actual structural property instead: it
/// reports the real, live org_events cursor from the moment a company exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_escalation_intents_are_always_found_and_never_hit_the_hardcoded_branch() {
    let w = world("oei-always-found").await;
    let ground_truth_seq = ground_truth_org_events_seq(&w).await;

    let (status, body) =
        post(&w.app, "/v1/org/operator-escalation-intents/read", json!({ "slug": w.slug })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let oei_read = parse(&body);
    assert_eq!(
        oei_read["found"], true,
        "operator_escalation_intents_read is unconditional and must always report found: true: {body}"
    );
    assert_eq!(oei_read["seq"].as_i64().unwrap(), ground_truth_seq, "{body}");
}
