#![allow(clippy::expect_used, clippy::panic)]

//! The Rust half of the conformance corpus, `session-maintenance` family —
//! plan §9, TESTING.md §2 and §7.
//!
//! Every fixture states `setup` → `op` → `expect` → `expectState`, recorded
//! from the **TypeScript**. The runner replays each one against the Rust store
//! and compares the bounded projection and every durable read byte for byte.
//!
//! # Why this file exists twice
//!
//! M10 landed a runner for this family against
//! `store/session_maintenance.rs`'s `Ledgers`-blob mutation verbs. Those verbs
//! had zero production callers and were deleted as a shadow twin (step 9), and
//! the runner went with them — `conformance/README.md` recorded the retarget
//! path and nothing has replayed these 44 fixtures in any language since. The
//! verbs are live now: [`chiefd_core::store::session_maintenance_ops`] is
//! called by `actor::session_lifecycle` and by eight
//! `/v1/org/session-maintenance/*` routes, and
//! the company-action store (deleted 2026-08-24) by
//! `actor::runtime_verbs`. This runner replays the corpus against **those**,
//! so what it proves is a property of production code.
//!
//! # The refusal taxonomy is mapped, not read (finding #751/G14)
//!
//! `conformance/FORMAT.md` says of a fixture's `expect.error.code`: *"This is
//! the field the Rust implementation must reproduce."* For `activity` and
//! `assignment` it does — `store/activity.rs` and `store/supervision.rs` carry
//! the corpus codes verbatim (`unknown-transition`, `fence-mismatch`), so
//! those runners read [`ChiefdError::code`] directly. The session-maintenance
//! port did **not**: it renamed every code into a `session-maintenance-*`
//! namespace, collapsed the corpus's fourteen distinct codes onto three
//! (`invalid-session-maintenance`, `unknown-session-maintenance-request`,
//! `session-maintenance-status-conflict`), and reports as `Refused` three
//! conditions the corpus records as `Conflict`.
//!
//! What the port DID preserve, byte for byte, is the refusal **prose**. So the
//! corpus identity is still recoverable, by exactly the procedure
//! the deleted `conformance/lib/taxonomy.ts` used on the TypeScript side: an
//! ordered table
//! of message rules. [`CORPUS_TAXONOMY`] is that table, ported rule for rule,
//! and an unmatched message is `unclassified` — which fails the runner loudly
//! rather than letting a new refusal path enter unnoticed.
//!
//! This is a mapping of a real divergence, not a repair of it. Making chiefd
//! answer the corpus codes on the wire changes eight live routes' response
//! bodies and three of their status classes, which is its own packet;
//! [`the_corpus_codes_chiefd_does_not_yet_carry`] pins the exact renames so
//! the debt is countable and so a fixed code shows up as a red test asking for
//! its row to be deleted.

mod conformance_common;

use std::collections::BTreeMap;

use chiefd_core::store::session_maintenance::{
    MaintenanceAction, MaintenanceRequest, MaintenanceStatus,
};
use chiefd_core::store::session_maintenance_ops::{
    self as maint_ops, Claim, CompactAnchor, ExpectedIdentity, FinishInput, QueueInput, StartInput,
};
use chiefd_core::ChiefdError;
use conformance_common::{
    assert_no_fixture_observes, assert_person_ids_come_from_the_template, expectation, integer,
    load_fixtures, optional_text, run_setup, sorted, text, Expectation, World,
};
use regex::Regex;
use serde_json::{json, Value};

const FAMILY: &str = "session-maintenance";

// TOMBSTONE: `FIXTURE_RUNTIME`. The company's runtime liveness, which only the
// company-action queue verb ever took.

// --- the corpus taxonomy ----------------------------------------------------

/// One rule: a refusal message pattern and the corpus `{type, code}` it means.
struct TaxonomyRule {
    /// Anchored pattern over the refusal's message.
    pattern: &'static str,
    /// The closed-taxonomy discriminant the corpus records.
    kind: &'static str,
    /// The language-neutral code the corpus records.
    code: &'static str,
}

/// the deleted `conformance/lib/taxonomy.ts`'s session-maintenance and shared
/// rules, as the recorded fixtures preserve them, in
/// order — first match wins, so specific rules sit above general ones.
///
/// Ported rule for rule rather than summarised: the two runners must make the
/// same classification decision, and the only way to see that they do is to be
/// able to diff them side by side.
const CORPUS_TAXONOMY: &[TaxonomyRule] = &[
    TaxonomyRule {
        pattern: r"^Unknown session maintenance request '.*'$",
        kind: "Refused",
        code: "unknown-request",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance request does not match the ChiefD-injected person$",
        kind: "Refused",
        code: "identity-echo-mismatch",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance request '.*' is not owned by this exact live claim$",
        kind: "Conflict",
        code: "claim-mismatch",
    },
    // #319 renamed the prose from "is not a forced company action" to "is not
    // a forced request"; the code — the stable taxonomy identity — did not
    // move, and chiefd carries the post-#319 prose.
    TaxonomyRule {
        pattern: r"^Session maintenance request '.*' is not a forced request$",
        kind: "Refused",
        code: "not-forced-company-action",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance request '.*' is no longer waiting for interruption$",
        kind: "Refused",
        code: "interrupt-window-closed",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance request '.*' is not a company native reset$",
        kind: "Refused",
        code: "not-company-native-reset",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance request '.*' was completed by another native compaction entry$",
        kind: "Conflict",
        code: "compaction-entry-conflict",
    },
    TaxonomyRule {
        pattern: r"^Company native reset '.*' has no exact source claim$",
        kind: "Refused",
        code: "no-source-claim",
    },
    TaxonomyRule {
        pattern: r"^Company native reset '.*' must complete from a different native Pi session$",
        kind: "Refused",
        code: "native-same-session",
    },
    TaxonomyRule {
        pattern: r"^Company native reset '.*' is not awaiting native session replacement$",
        kind: "Refused",
        code: "native-not-running",
    },
    TaxonomyRule {
        pattern: r"^Company native reset '.*' was completed by another exact target session$",
        kind: "Conflict",
        code: "native-completion-conflict",
    },
    TaxonomyRule {
        pattern: r"^A native compaction entry may only complete anchored compact maintenance$",
        kind: "Refused",
        code: "compaction-entry-not-anchored",
    },
    TaxonomyRule {
        pattern: r"^Only compact maintenance can persist a native compact anchor$",
        kind: "Refused",
        code: "anchor-requires-compact",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance person is unknown$",
        kind: "Refused",
        code: "unknown-person",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance action is invalid$",
        kind: "Refused",
        code: "invalid-input",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance (claim|deferral|interrupt|recovery)\.?\w* ?processId must be a positive integer$",
        kind: "Refused",
        code: "invalid-input",
    },
    TaxonomyRule {
        pattern: r"^Native fresh-session completion processId must be a positive integer$",
        kind: "Refused",
        code: "invalid-input",
    },
    TaxonomyRule {
        pattern: r"^Session maintenance [a-z]+\.[A-Za-z]+ (is required|must .*)$",
        kind: "Refused",
        code: "invalid-input",
    },
    TaxonomyRule {
        pattern: r"^session maintenance ?\.?[A-Za-z]+ (is required|must .*)$",
        kind: "Refused",
        code: "invalid-input",
    },
    // TOMBSTONE: two company-session-action rules -- the already-forced /
    // already-cooperative mode conflict, and "must be compact or fresh_session".
    // No producer of either string survives in `src`, and the fixtures that
    // recorded them are deleted. An expectation row whose subject is gone
    // passes green for ever while asserting nothing.
    // ---- shared ------------------------------------------------------------
    TaxonomyRule {
        pattern: r"^Person '.*' has a fresh session transition in progress$",
        kind: "Refused",
        code: "fresh-session-in-progress",
    },
    TaxonomyRule {
        pattern: r"^Person '.*' has open work and cannot start a fresh session$",
        kind: "Refused",
        code: "open-work-present",
    },
    TaxonomyRule {
        pattern: r"^Unknown organization person '.*'$",
        kind: "Refused",
        code: "unknown-person",
    },
    TaxonomyRule {
        pattern: r"^Person '.*' is not active$",
        kind: "Refused",
        code: "person-not-active",
    },
    TaxonomyRule {
        pattern: r"^Unknown organization '.*'$",
        kind: "Refused",
        code: "unknown-company",
    },
];

/// The message a store refusal carries, or a panic naming the shape that has no
/// message to classify.
fn refusal_message(error: &ChiefdError) -> String {
    match error {
        ChiefdError::Refused(refusal) => refusal.message.clone(),
        other => panic!(
            "the session-maintenance verbs refuse; this runner saw {other:?}, which the corpus \
             taxonomy has no rule shape for. A new error variant on this path is a wire change \
             and needs a decision, not a default."
        ),
    }
}

/// Classify a refusal message onto the corpus's `{type, code}`.
///
/// Unclassified is deliberately fatal, exactly as `taxonomy.ts` makes it: a
/// refusal nobody has decided the meaning of cannot guide the port, so it must
/// not enter a passing run silently.
fn classify(message: &str) -> (String, String) {
    for rule in CORPUS_TAXONOMY {
        let pattern = Regex::new(rule.pattern).expect("a taxonomy rule is a valid regex");
        if pattern.is_match(message) {
            return (rule.kind.to_string(), rule.code.to_string());
        }
    }
    panic!(
        "refusal '{message}' matches no rule in CORPUS_TAXONOMY, so it would be recorded \
         `unclassified`. `conformance/FORMAT.md`: an unclassified refusal cannot guide the port, \
         so it must not enter the corpus silently — add the rule and say what it means."
    );
}

/// A store error, as the corpus's `{type, code}` pair.
fn taxonomy(error: &ChiefdError) -> (String, String) {
    classify(&refusal_message(error))
}

// TOMBSTONE: `refusal_taxonomy`. It classified the decision-only `Refusal` that
// `queue_company_action` returned; every surviving op returns `ChiefdError`.

// --- projections: these must match the recorded fixtures exactly -----------
// (`conformance/lib/ops.ts` held the TypeScript read registry and is deleted;
//  the fixtures ARE the contract now, and every one is compared byte for byte.)

/// The bounded projection of a maintenance request.
///
/// The value-action payloads (`set_thinking`/`set_model`) are emitted ONLY when
/// present, so a `compact`/`fresh_session` view is byte-unchanged and a
/// value-action fixture asserts just the key it carries. That mirrors both
/// `requestView` in the deleted `conformance/lib/ops.ts` — preserved by the
/// fixtures it recorded — and serde's
/// `skip_serializing_if` on the DTO.
fn request_view(request: &MaintenanceRequest) -> Value {
    let view = json!({
        "id": request.id,
        "action": request.action.as_str(),
        "personId": request.person_id,
        "status": status_str(request.status),
        "attempt": request.attempt,
        "automatic": request.automatic,
        "requestedBy": request.requested_by,
        "force": request.force == Some(true),
        "recoveredFromRequestId": request.recovered_from_request_id,
        "retryNotBefore": request.retry_not_before,
        "claimedProcessId": request.claimed_process_id,
        "claimedSessionId": request.claimed_session_id,
        "claimToken": request.claim_token,
        "completedSessionId": request.completed_session_id,
        "interruptedSessionId": request.interrupted_session_id,
        "compactSessionId": request.compact_session_id,
        "compactAnchorEntryId": request.compact_anchor_entry_id,
        "completedCompactionEntryId": request.completed_compaction_entry_id,
        "error": request.error,
    });
    view
}

const fn status_str(status: MaintenanceStatus) -> &'static str {
    match status {
        MaintenanceStatus::Queued => "queued",
        MaintenanceStatus::Running => "running",
        MaintenanceStatus::Applying => "applying",
        MaintenanceStatus::Completed => "completed",
        MaintenanceStatus::Failed => "failed",
        MaintenanceStatus::Skipped => "skipped",
    }
}

// --- fixture input helpers --------------------------------------------------

/// An action a fixture names.
///
/// `Err` is the corpus's invalid-action refusal. In chiefd that refusal happens
/// at the schema boundary — [`MaintenanceAction`] has no variant to hold it, and
/// the route bodies are `deny_unknown_fields` — before any state is touched, so
/// the message the boundary reports is classified through the same table every
/// other refusal goes through rather than being special-cased into a code.
fn action_of(input: &Value) -> Result<MaintenanceAction, (String, String)> {
    match input.get("action").and_then(Value::as_str) {
        Some("compact") => Ok(MaintenanceAction::Compact),
        _ => Err(classify("Session maintenance action is invalid")),
    }
}

fn claim_of(input: &Value) -> Claim {
    let claim = &input["claim"];
    Claim {
        process_id: integer(claim, "processId"),
        session_id: text(claim, "sessionId"),
        claim_token: text(claim, "claimToken"),
    }
}

/// The identity a request-naming verb presents.
///
/// The TypeScript made the attested identity OPTIONAL: `expectedIdentity()`
/// spread nothing when a fixture carried no `caller`, and the wrapper skipped
/// the check entirely. chiefd made it mandatory — every
/// `/v1/org/session-maintenance/*` body carries `identity` — so "no fence
/// presented" has no representation there. The runner reproduces the TS path
/// exactly by presenting the fence the named request itself holds, which is the
/// one identity that can never refuse. A fixture that DOES carry a caller
/// presents it verbatim, so the mismatch fixtures still refuse.
fn identity_for(world: &World, request_id: &str, caller: Option<&Value>) -> ExpectedIdentity {
    if let Some(person_id) = caller.and_then(|c| c.get("personId")).and_then(Value::as_str) {
        return ExpectedIdentity { person_id: person_id.to_string() };
    }
    world.maintenance().request(request_id).map_or_else(
        || {
            // No such request. Every verb below resolves the request before it
            // checks the fence, so this identity is never compared against
            // anything — the refusal is `unknown-request` either way.
            ExpectedIdentity { person_id: "nobody".to_string() }
        },
        |request| ExpectedIdentity { person_id: request.person_id.clone() },
    )
}

/// The identity a person-scoped verb (`start`, `recover`) presents, which the
/// corpus carries in `in` rather than in `caller`.
fn person_identity(input: &Value) -> ExpectedIdentity {
    ExpectedIdentity { person_id: text(input, "personId") }
}

fn finish_status(input: &Value) -> MaintenanceStatus {
    match text(input, "status").as_str() {
        "completed" => MaintenanceStatus::Completed,
        "failed" => MaintenanceStatus::Failed,
        "skipped" => MaintenanceStatus::Skipped,
        other => panic!("fixture names unknown finish status '{other}'"),
    }
}

// --- the op registry --------------------------------------------------------

/// Run one op and record whether it wrote the session-maintenance document.
///
/// The corpus's `maint.summary` distinguishes an ABSENT ledger from an empty
/// one, so the runner has to know which verbs actually wrote. At this layer
/// that is decidable exactly: a verb wrote iff the ledger value changed.
///
/// chiefd's *actor* is slightly looser — `CompanyDb::maintenance_mutation`
/// stages the ledger on every successful verb, so a bounded idle probe that
/// claims nothing materializes an empty maintenance document where the
/// TypeScript left none. Nothing in this family's reads can tell an empty
/// ledger from an absent one afterwards (`maint.request` answers `null` either
/// way), so it is a
/// difference in when the row appears, not in what any caller can observe —
/// recorded here rather than silently absorbed.
fn run_op(
    world: &mut World,
    op: &str,
    input: &Value,
    caller: Option<&Value>,
) -> Result<Value, (String, String)> {
    let before = world.maintenance.clone();
    let outcome = dispatch_op(world, op, input, caller);
    world.observe_maintenance_write(before.as_ref());
    outcome
}

fn dispatch_op(
    world: &mut World,
    op: &str,
    input: &Value,
    caller: Option<&Value>,
) -> Result<Value, (String, String)> {
    let at = world.now_iso();
    match op {
        "company.create" => Ok(world.create_company(&text(input, "template"))),
        "clock.advance" => {
            world.advance(integer(input, "milliseconds"));
            Ok(json!({ "now": world.now_iso() }))
        }
        "maint.queue" => {
            let action = action_of(input)?;
            assert!(
                input.get("modelTaskClass").is_none(),
                "no recorded fixture supplies a model task class; chiefd's QueueInput does not \
                 carry it, so wire it through before a fixture depends on it"
            );
            let queue = QueueInput {
                action,
                person_id: text(input, "personId"),
                requested_by: text(input, "requestedBy"),
                reason: text(input, "reason"),
                automatic: input.get("automatic").and_then(Value::as_bool).unwrap_or(false),
                force: input.get("force").and_then(Value::as_bool),
            };
            let manifest = world.manifest().clone();
            maint_ops::queue(world.maintenance_mut(), &manifest, &queue, &at)
                .map(|request| request_view(&request))
                .map_err(|error| taxonomy(&error))
        }
        "maint.start" => {
            let action = action_of(input)?;
            let identity = person_identity(input);
            let request_id =
                optional_text(input, "requestId").map(|id| resolved_request_id(world, &id));
            let claim = input.get("claim").map(|_| claim_of(input));
            let anchor = input.get("compactAnchor").map(|anchor| CompactAnchor {
                session_id: text(anchor, "sessionId"),
                entry_id: text(anchor, "entryId"),
            });
            maint_ops::start(
                world.maintenance_mut(),
                &identity,
                &StartInput {
                    action,
                    request_id: request_id.as_deref(),
                    claim: claim.as_ref(),
                    compact_anchor: anchor.as_ref(),
                },
                &at,
            )
            .map(|claimed| claimed.as_ref().map_or(Value::Null, request_view))
            .map_err(|error| taxonomy(&error))
        }
        "maint.defer" => {
            let request_id = resolved_request_id(world, &text(input, "requestId"));
            let claim = claim_of(input);
            let identity = identity_for(world, &request_id, caller);
            maint_ops::defer(world.maintenance_mut(), &request_id, &claim, &identity, &at)
                .map(|request| request_view(&request))
                .map_err(|error| taxonomy(&error))
        }
        "maint.interrupt" => {
            let request_id = resolved_request_id(world, &text(input, "requestId"));
            let claim = claim_of(input);
            let identity = identity_for(world, &request_id, caller);
            maint_ops::record_interrupt(
                world.maintenance_mut(),
                &request_id,
                &claim,
                &identity,
                &at,
            )
            .map(|request| request_view(&request))
            .map_err(|error| taxonomy(&error))
        }
        "maint.recover" => {
            let identity = person_identity(input);
            let claim = claim_of(input);
            maint_ops::recover_interrupted(world.maintenance_mut(), &identity, &claim, &at)
                .map(|recovered| {
                    json!({
                        "interrupted": recovered
                            .interrupted
                            .iter()
                            .map(request_view)
                            .collect::<Vec<_>>(),
                        "replacements": recovered
                            .replacements
                            .iter()
                            .map(request_view)
                            .collect::<Vec<_>>(),
                    })
                })
                .map_err(|error| taxonomy(&error))
        }
        "maint.finish" => {
            let request_id = resolved_request_id(world, &text(input, "requestId"));
            let status = finish_status(input);
            let error = optional_text(input, "error");
            let compact_entry_id = optional_text(input, "compactEntryId");
            let identity = identity_for(world, &request_id, caller);
            maint_ops::finish(
                world.maintenance_mut(),
                &FinishInput {
                    id: &request_id,
                    status,
                    error: error.as_deref(),
                    compact_entry_id: compact_entry_id.as_deref(),
                },
                &identity,
                &at,
            )
            .map(|request| request_view(&request))
            .map_err(|error| taxonomy(&error))
        }
        other => panic!(
            "fixture names op '{other}', which the Rust runner cannot execute. \
             `conformance/FORMAT.md`: a fixture the Rust runner cannot execute is a \
             missing chiefd verb, and it should be loud."
        ),
    }
}

/// Translate a fixture's RECORDED request id into the one this store minted.
///
/// # Why the corpus is not rewritten instead
///
/// The recorded ids are `session-maintenance:<position>:<person>:<action>`,
/// the counted shape the TypeScript minted. chiefd's id is now a HASH OF THE
/// REQUEST'S CONTENT, because a replayed tool call must reuse its request
/// rather than queue a second one — for `fresh_session`, a second request is a
/// second REAL pane restart. That ruling is the subject of its own tests; it is
/// not what these 36 fixtures record.
///
/// What they record is BEHAVIOUR: which refusal a foreign claim gets, what a
/// defer does to durable state, which fields survive a completion. Coupling
/// every one of those recordings to the id FORMAT was incidental, and pasting
/// 36 sha-256 digests into the corpus would make the recordings unreadable and
/// re-brittle them against the next content change. So the legacy shape is
/// resolved to the live request by the two things the id is now derived from —
/// the person and the action it names.
///
/// It resolves ONLY the legacy shape, and only when exactly one request
/// matches. An id that is already a content hash passes through untouched, and
/// an unresolvable one is passed through as written so the store answers
/// `unknown-request` exactly as it would in production — a translation that
/// invented a match would turn a real "no such request" into a false success.
fn resolved_request_id(world: &World, recorded: &str) -> String {
    let mut parts = recorded.splitn(4, ':');
    let (Some("session-maintenance"), Some(position_segment), Some(person_id), Some(action)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return recorded.to_owned();
    };
    if position_segment.parse::<u64>().is_err() {
        // A content hash, not the counted legacy shape.
        return recorded.to_owned();
    }
    // `<N>` was the request's 1-BASED POSITION in `request_order` at mint time
    // (`request_order.len() + 1`), so it addresses the Nth request and not the
    // Nth of this person's. That matters for the recovery fixtures, where three
    // requests share one person and action and only the position tells them
    // apart.
    //
    // The position is CHECKED rather than trusted: the request found there must
    // still name the recorded person and action, or the recorded id is not
    // describing this request and is passed through untranslated.
    let order = &world.maintenance().request_order;
    let Ok(position) = position_segment.parse::<usize>() else { return recorded.to_owned() };
    let Some(id) = position.checked_sub(1).and_then(|index| order.get(index)) else {
        return recorded.to_owned();
    };
    let Some(request) = world.maintenance().request(id) else { return recorded.to_owned() };
    let same_action = serde_json::to_value(request.action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .as_deref()
        == Some(action);
    if request.person_id == person_id && same_action {
        return id.clone();
    }
    recorded.to_owned()
}

/// The same translation, applied to the EXPECTED side of a comparison.
///
/// The recorded ids appear inside recorded VALUES too — a request's own `id`
/// field, and `requestOrder` — so translating only the inputs leaves every
/// comparison failing on the one field the ruling deliberately changed. Walks
/// the whole recorded value so an id is translated wherever it appears, and
/// touches nothing else: any string that is not the legacy shape, and any
/// legacy-shaped string with no single matching request, is left exactly as
/// recorded.
fn translate_recorded_ids(world: &World, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(resolved_request_id(world, text)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| translate_recorded_ids(world, item)).collect())
        }
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), translate_recorded_ids(world, item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn run_read(world: &World, read: &str, args: &Value) -> Value {
    let ledger = world.maintenance();
    match read {
        // Addressable two ways, and the second is not a convenience.
        //
        // A fixture that names `requestId` pins the id VERBATIM, which is right
        // for every recording whose subject includes the id. But a request's id
        // is now a hash of its CONTENT (`session_maintenance_ops::
        // request_identity`), so it cannot be written down in a static fixture
        // at all -- and a recording whose subject is the durable STATE after an
        // operation should not have been coupled to the id shape in the first
        // place. Such a fixture names `personId` + `action` instead, which
        // identifies exactly the same single request by exactly the thing the
        // id is now derived from.
        //
        // It resolves to at most one request on purpose: if two ever matched,
        // the read is ambiguous and the fixture is asserting against whichever
        // came first, so it panics rather than picking.
        "maint.request" => {
            if let Some(request_id) = args.get("requestId").and_then(Value::as_str) {
                let resolved = resolved_request_id(world, request_id);
                return world.maintenance().request(&resolved).map_or(Value::Null, request_view);
            }
            let person_id = text(args, "personId");
            let action = text(args, "action");
            let matched: Vec<_> = ledger
                .ordered_requests()
                .filter(|request| {
                    request.person_id == person_id
                        && serde_json::to_value(request.action)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            == Some(action.clone())
                })
                .collect();
            assert!(
                matched.len() <= 1,
                "`maint.request` by person+action matched {} requests for '{person_id}'/'{action}' \
                 — the read is ambiguous and would assert against whichever sorted first",
                matched.len()
            );
            matched.first().map_or(Value::Null, |request| request_view(request))
        }
        "maint.summary" => {
            if world.maintenance_written {
                json!({ "requestOrder": ledger.request_order })
            } else {
                Value::Null
            }
        }
        // TOMBSTONE: the `maint.company_action_target` read.
        other => panic!("fixture names read '{other}', which the Rust runner cannot execute"),
    }
}

// --- the runner -------------------------------------------------------------

#[test]
fn every_session_maintenance_fixture_replays_against_the_rust_store() {
    let fixtures = load_fixtures(FAMILY);
    let mut replayed = 0_usize;

    for (name, fixture) in &fixtures {
        let mut world = World::new();
        run_setup(&mut world, name, fixture, run_op);

        let op = fixture["op"].as_str().expect("an op");
        let input = fixture.get("in").cloned().unwrap_or_else(|| json!({}));
        let observed = run_op(&mut world, op, &input, fixture.get("caller"));

        match expectation(fixture) {
            Expectation::Ok(recorded) => {
                let value = observed.unwrap_or_else(|(kind, code)| {
                    panic!("{name}: expected ok, got {kind}/{code}")
                });
                assert_eq!(
                    sorted(&value),
                    sorted(&translate_recorded_ids(&world, &recorded)),
                    "{name}: response projection differs"
                );
            }
            Expectation::Error { kind, code } => match observed {
                Ok(value) => panic!("{name}: expected {kind}/{code}, got ok: {value}"),
                Err((observed_kind, observed_code)) => assert_eq!(
                    (observed_kind.as_str(), observed_code.as_str()),
                    (kind.as_str(), code.as_str()),
                    "{name}: taxonomy differs"
                ),
            },
        }

        let expect_state = fixture["expectState"].as_array().expect("`expectState` is an array");
        assert!(!expect_state.is_empty(), "{name}: a fixture must assert durable state");
        for expectation in expect_state {
            let read = expectation["read"].as_str().expect("a read name");
            let args = expectation.get("args").cloned().unwrap_or_else(|| json!({}));
            let observed = run_read(&world, read, &args);
            assert_eq!(
                sorted(&observed),
                sorted(&translate_recorded_ids(&world, &expectation["equals"])),
                "{name}: durable read '{read}' differs"
            );
        }
        replayed += 1;
    }

    // The count is asserted against the corpus on disk, not against a number
    // written here: a runner that silently skipped what it did not understand
    // would be green for the same reason a runner that never ran is green.
    assert_eq!(
        replayed,
        fixtures.len(),
        "the runner replayed {replayed} of {} fixtures on disk",
        fixtures.len()
    );
    println!("{replayed}/{} session-maintenance fixtures replayed", fixtures.len());
}

/// Every op and read the corpus names must be one this runner executes.
///
/// The runner itself panics on an unknown op, but only on the fixtures it
/// reaches; this walks the whole corpus, including `setup` steps, so a family
/// that grew a verb chiefd does not have is reported as one list rather than as
/// the first failure.
#[test]
fn every_op_and_read_the_corpus_names_is_one_this_runner_executes() {
    const OPS: &[&str] = &[
        "company.create",
        "clock.advance",
        "maint.queue",
        "maint.start",
        "maint.defer",
        "maint.interrupt",
        "maint.recover",
        "maint.finish",
    ];
    // `maint.company_action_target` was the third read; it pointed at a company
    // action's current request and is deleted with the family.
    const READS: &[&str] = &["maint.request", "maint.summary"];

    let mut missing_ops: Vec<String> = Vec::new();
    let mut missing_reads: Vec<String> = Vec::new();
    for (name, fixture) in load_fixtures(FAMILY) {
        let mut ops: Vec<String> = fixture["setup"]
            .as_array()
            .expect("`setup` is an array")
            .iter()
            .map(|step| step["op"].as_str().expect("a setup op").to_string())
            .collect();
        ops.push(fixture["op"].as_str().expect("an op").to_string());
        for op in ops {
            if !OPS.contains(&op.as_str()) {
                missing_ops.push(format!("{name}: {op}"));
            }
        }
        for state in fixture["expectState"].as_array().expect("`expectState` is an array") {
            let read = state["read"].as_str().expect("a read name");
            if !READS.contains(&read) {
                missing_reads.push(format!("{name}: {read}"));
            }
        }
    }
    assert!(
        missing_ops.is_empty() && missing_reads.is_empty(),
        "the corpus names verbs this runner cannot execute.\n  ops: {missing_ops:?}\n  reads: \
         {missing_reads:?}\nFORMAT.md: a fixture the Rust runner cannot execute is a missing \
         chiefd verb, and it should be loud."
    );
}

/// A guard against the runner rotting into a description of nothing.
#[test]
fn the_corpus_still_pins_the_invariants_this_family_owes() {
    let names: Vec<String> = load_fixtures(FAMILY).into_iter().map(|(name, _)| name).collect();
    // TWO OF THE FOUR NAMED HERE ARE DELETED, and they were named because they
    // were load-bearing — which they were, for a feature that no longer exists.
    // `inv12-historical-failed-request-is-not-recovered-without-the-flag`
    // pinned that a FAILED company reset is invisible to recovery without its
    // flag; `company-action-fans-out-one-request-per-person` pinned the fanout
    // itself. Both exercised the company-action family, deleted whole on
    // 2026-08-24 because nothing in production could queue one.
    //
    // The two that remain still guard what this list is for: that the runner
    // cannot rot into a description of nothing.
    for required in [
        "inv26-start-with-no-work-is-the-literal-null",
        "recover-of-a-plain-request-is-bounded-to-three-attempts",
    ] {
        assert!(
            names.iter().any(|name| name == required),
            "the corpus lost '{required}', which is one of this family's load-bearing fixtures"
        );
    }
    // 41 -> 25 with the company-action and native-reset fixtures. This is a
    // FLOOR with headroom rather than the count, deliberately: a floor sitting
    // ON the real number fails the next time anything shrinks, which this
    // repo's route-literal ceiling learned three times over.
    assert!(names.len() >= 20, "the session-maintenance family lost fixtures: {}", names.len());
}

/// The corpus's one declared divergence, and the fact that chiefd has not taken
/// it yet.
///
/// `PLAN-DELTA-start-replay-of-the-same-claim-returns-null-today` records that
/// re-presenting the SAME claim triple answers `null`, and plan §2.5 marks it
/// `[Δ]`: chiefd is supposed to return the already-claimed request so that
/// `null` can mean "no work AND you hold nothing". `session_maintenance_ops::
/// start` matches only `Queued` requests, so chiefd answers `null` — the
/// divergence is **not implemented**, and the main runner therefore asserts
/// exact equality on this fixture like any other.
///
/// That is a legitimate state to be in, but it must be a stated one: a
/// PLAN-DELTA fixture that quietly passes as an exact match is indistinguishable
/// from a divergence nobody is testing. When §2.5 lands, this test goes red and
/// says what to do.
#[test]
fn the_one_declared_plan_divergence_is_recorded_as_outstanding() {
    const DELTA: &str = "PLAN-DELTA-start-replay-of-the-same-claim-returns-null-today";
    let deltas: Vec<String> = load_fixtures(FAMILY)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("PLAN-DELTA-"))
        .collect();
    assert_eq!(
        deltas,
        vec![DELTA.to_string()],
        "the set of declared divergences changed; each one needs a decision recorded here"
    );

    let mut world = World::new();
    world.create_company("northstar");
    let queue = QueueInput {
        action: MaintenanceAction::Compact,
        person_id: "signal-researcher".to_string(),
        requested_by: "signal-researcher".to_string(),
        reason: "Context is nearly full.".to_string(),
        automatic: false,
        force: None,
    };
    let manifest = world.manifest().clone();
    let at = world.now_iso();
    maint_ops::queue(world.maintenance_mut(), &manifest, &queue, &at).expect("the request queues");
    let identity = ExpectedIdentity { person_id: "signal-researcher".to_string() };
    let claim = Claim {
        process_id: 4242,
        session_id: "session-alpha".to_string(),
        claim_token: "token-alpha".to_string(),
    };
    let start = StartInput {
        action: MaintenanceAction::Compact,
        request_id: None,
        claim: Some(&claim),
        compact_anchor: None,
    };
    maint_ops::start(world.maintenance_mut(), &identity, &start, &at)
        .expect("the first claim succeeds")
        .expect("the first claim returns the request");
    let replay = maint_ops::start(world.maintenance_mut(), &identity, &start, &at)
        .expect("a claim replay is not an error");
    assert!(
        replay.is_none(),
        "chiefd now returns the already-claimed request on a same-claim replay, which is plan \
         §2.5 [Δ] landing. Re-record '{DELTA}' against chiefd, drop the PLAN-DELTA prefix, and \
         delete this test — the divergence is closed."
    );
}

/// The exact corpus codes chiefd does not carry, and what it answers instead.
///
/// This is the ledger of the #751/G14 finding described in the module docs. It
/// is not a workaround: [`CORPUS_TAXONOMY`] recovers the corpus identity from
/// the (byte-identical) refusal prose, so the corpus replays. What this table
/// records is that a CLIENT of `/v1/org/session-maintenance/*` cannot make the
/// same distinction, because chiefd puts three codes on the wire where the
/// corpus has fourteen, and reports `Refused` where the corpus says `Conflict`.
///
/// Deleting a row here means chiefd started carrying that code — which is the
/// fix, and which this test then demands be proven by removing the row.
#[test]
fn the_corpus_codes_chiefd_does_not_yet_carry() {
    // corpus code -> (corpus type, the code chiefd's Refusal actually carries)
    let debt: BTreeMap<&str, (&str, &str)> = BTreeMap::from([
        ("unknown-request", ("Refused", "unknown-session-maintenance-request")),
        ("identity-echo-mismatch", ("Refused", "session-maintenance-identity-mismatch")),
        ("claim-mismatch", ("Conflict", "session-maintenance-claim-mismatch")),
        ("not-forced-company-action", ("Refused", "session-maintenance-status-conflict")),
        ("interrupt-window-closed", ("Refused", "session-maintenance-status-conflict")),
        ("not-company-native-reset", ("Refused", "session-maintenance-status-conflict")),
        ("compaction-entry-conflict", ("Conflict", "session-maintenance-status-conflict")),
        ("no-source-claim", ("Refused", "session-maintenance-status-conflict")),
        ("native-same-session", ("Refused", "session-maintenance-status-conflict")),
        ("native-completion-conflict", ("Conflict", "session-maintenance-status-conflict")),
        ("compaction-entry-not-anchored", ("Refused", "invalid-session-maintenance")),
        ("anchor-requires-compact", ("Refused", "invalid-session-maintenance")),
        ("unknown-person", ("Refused", "invalid-session-maintenance")),
        ("invalid-input", ("Refused", "invalid-session-maintenance")),
        // TOMBSTONE: `company-action-mode-conflict`, whose rule above is deleted.
    ]);

    // Every code the corpus actually records for this family must be in the
    // ledger above, so the debt cannot be under-reported by a fixture landing
    // a code nobody wrote down.
    let mut recorded: Vec<(String, String)> = load_fixtures(FAMILY)
        .into_iter()
        .filter_map(|(_, fixture)| {
            let error = fixture["expect"].get("error")?;
            Some((error["type"].as_str()?.to_string(), error["code"].as_str()?.to_string()))
        })
        .collect();
    recorded.sort();
    recorded.dedup();
    for (kind, code) in &recorded {
        let (expected_kind, chiefd_code) = debt.get(code.as_str()).unwrap_or_else(|| {
            panic!(
                "fixtures record refusal code '{code}', which is not in the G14 debt ledger. \
                 Either chiefd now carries it — delete nothing, add the row saying so — or the \
                 divergence grew and needs writing down."
            )
        });
        assert_eq!(
            kind, expected_kind,
            "the corpus records '{code}' as {kind}, the ledger says {expected_kind}"
        );
        assert_ne!(
            code, chiefd_code,
            "chiefd now carries the corpus code '{code}'. Delete this row and switch that rule to \
             reading ChiefdError::code() directly — the mapping for it is no longer needed."
        );
    }
}

/// Every rule in the mapping table is a valid, anchored regex, and no two rules
/// claim the same message.
#[test]
fn the_corpus_taxonomy_table_is_well_formed() {
    for rule in CORPUS_TAXONOMY {
        assert!(Regex::new(rule.pattern).is_ok(), "rule '{}' is not a valid regex", rule.pattern);
        assert!(
            rule.pattern.starts_with('^'),
            "rule '{}' is unanchored and could match a substring of an unrelated refusal",
            rule.pattern
        );
        assert!(
            matches!(
                rule.kind,
                "Refused" | "Conflict" | "Busy" | "StoreFailure" | "Corrupt" | "Unavailable"
            ),
            "rule '{}' names '{}', which is outside the closed taxonomy",
            rule.pattern,
            rule.kind
        );
    }
}

/// The template's placeholder model facts must not become load-bearing here
/// either — but `model` itself is excluded from the list, because this family's
/// `set_model` fixtures carry a model the CALLER chose. That value is request
/// payload the corpus must pin; `provider`, `taskClass` and `modelReason` are
/// the template-derived facts the northstar template only placeholds.
#[test]
fn no_session_maintenance_fixture_depends_on_the_model_catalog() {
    assert_no_fixture_observes(FAMILY, &["provider", "taskClass", "modelReason"]);
}

#[test]
fn the_northstar_template_matches_what_the_fixtures_were_recorded_against() {
    assert_person_ids_come_from_the_template(FAMILY);
}
