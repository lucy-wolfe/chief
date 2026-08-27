//! The `AGENTS.md` role-gate exception and the route that justified it cannot
//! drift apart — asserted in BOTH DIRECTIONS.
//!
//! # What this exists to stop
//!
//! `AGENTS.md` carried a paragraph naming `org_lifecycle_status`,
//! `org_maintain_session` and `org_set_thinking` as deliberate exceptions to
//! the rule that authority is the subtree you head and never the job title.
//! Its justification was a fact about a ROUTE: `/v1/org/session-maintenance/queue`
//! took no caller identity at all — `MaintenanceQueueRequest` had no identity
//! field, unlike `MaintenanceStartRequest` beside it — so the TypeScript kind
//! check WAS the authorization rather than a pre-flight in front of one.
//!
//! Doc and code then drifted, twice, and nothing caught either:
//!
//! * the paragraph cited `chiefd-api/tests/verb_authorization_table.rs` as its
//!   evidence long after track C1 deleted that file with `VerbAuth`;
//! * it claimed `scopeDepartmentId` was "a filter the caller chooses, never a
//!   fence the server applies" after B4 made the server derive it.
//!
//! Both were prose asserting a code fact, with no guard between them. This is
//! that guard.
//!
//! # Why BOTH directions
//!
//! A one-directional check would let the doc come back. If this only asserted
//! "the paragraph is absent", somebody could re-add the exception prose and the
//! suite would stay green while the route it describes stayed fenced. If it
//! only asserted "the route is fenced", the paragraph could rot again the way
//! it just did. So the guard pins the PAIR:
//!
//! * the route binds its requester to the authenticated caller, AND
//! * `AGENTS.md` claims no role-gate exception.
//!
//! Either one alone is a half-truth. Both together are the state this commit
//! established, and a future packet that legitimately reverses it has to
//! reverse both halves here, in one edit, on purpose.

#![allow(clippy::expect_used, clippy::panic)]

/// `AGENTS.md`, read from the repository root rather than copied, so this
/// cannot pass against a stale duplicate. `CLAUDE.md` is a symlink to it, so
/// one file is the whole surface.
fn agents_md() -> String {
    // `CARGO_MANIFEST_DIR` is `<repo>/apps/chiefd/crates/chiefd-api`, so the
    // repo root is FOUR ancestors up: chiefd-api → crates → chiefd → apps →
    // root. Counted rather than guessed, because an off-by-one here reads a
    // file that does not exist and the guard fails for the wrong reason.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("repo root is four levels above chiefd-api")
        .join("AGENTS.md");
    std::fs::read_to_string(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
}

/// The router source, which is where the route's shape actually lives.
fn router_rs() -> &'static str {
    include_str!("../src/docstore/router.rs")
}

/// DIRECTION ONE: the route is fenced.
///
/// `requested_by` arrives in the body, and the handler reconciles it with the
/// authenticated caller. Without this line the field is caller-asserted and
/// the whole exception paragraph would be true again.
#[test]
fn the_maintenance_queue_route_binds_its_requester_to_the_caller() {
    let handler = router_rs()
        .split("async fn org_session_maintenance_queue")
        .nth(1)
        .expect("org_session_maintenance_queue is defined in router.rs");
    let body = handler.split("\n}\n").next().unwrap_or(handler);
    let code: String = body.lines().filter(|line| !line.trim_start().starts_with("//")).collect();
    assert!(
        // `&caller`, not `caller.as_ref()`: the helper takes `&Identity` now
        // that the absent-caller arm is deleted, so there is no `Option` left
        // to unwrap. The binding itself — the thing this guard is about — is
        // unchanged.
        code.contains("bind_caller(&caller, Some(req.requested_by.as_str()), &req.slug)?"),
        "maint.queue must bind its declared requester to the authenticated caller; without \
         that binding the AGENTS.md role-gate exception becomes true again and must be restored \
         in the same commit that removes this binding"
    );
}

/// DIRECTION TWO: the doc claims no exception.
///
/// Keyed on the SHAPE of the claim rather than on one sentence, because the
/// sentence will be edited again. Any prose reinstating a `manager()` check for
/// those three tools has to say so, and saying so is what this catches.
#[test]
fn agents_md_claims_no_role_gate_exception() {
    let doc = agents_md();
    for banned in
        ["keep a `manager()` check", "three tools are deliberate", "verb_authorization_table.rs"]
    {
        assert!(
            !doc.contains(banned),
            "AGENTS.md still says {banned:?}. The role-gate exception was deleted when the \
             routes behind it became fenced; re-asserting it here without also un-fencing the \
             route is exactly the drift this guard exists to stop"
        );
    }
}

/// The pair, stated as one fact so a reader sees the coupling rather than two
/// unrelated tests that happen to live in one file.
///
/// This is the assertion the operator asked for: the paragraph exists exactly
/// while the route is unfenced, and neither is true today.
#[test]
fn the_doc_and_the_route_agree_that_there_is_no_exception() {
    let doc_claims_exception = agents_md().contains("deliberate exceptions to it");
    let route_is_fenced = {
        let handler = router_rs()
            .split("async fn org_session_maintenance_queue")
            .nth(1)
            .expect("handler present");
        handler.split("\n}\n").next().unwrap_or(handler).contains("bind_caller(")
    };
    assert!(
        doc_claims_exception != route_is_fenced,
        "AGENTS.md and the route must never agree: an exception paragraph is only honest while \
         the route is unfenced. doc_claims_exception={doc_claims_exception}, \
         route_is_fenced={route_is_fenced}"
    );
    assert!(route_is_fenced, "the route is fenced, so the doc must claim no exception");
}
