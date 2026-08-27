#![allow(clippy::expect_used, clippy::panic)]

//! The `tools` family of the conformance corpus — **4 fixtures, all replayed
//! against Rust**, and the mechanical proof that each still has a live runner.
//!
//! # The 121 that used to sit here are gone
//!
//! This family carried 137 fixtures, of which 121 had NO Rust subject and were
//! quarantined: `tools.describe` is registration, schemas and cards, and none
//! of that exists in `apps/chiefd` to replay against. They were also
//! unrecordable — #1047 deleted the TypeScript harness (`record-ts.ts`,
//! `run-ts.ts`, `lib/`, `scenarios/`) and nothing replaced it — so a stale one
//! could only ever be repaired by hand.
//!
//! They were deleted on the operator's ruling, because a guard written against
//! them measured what a corpus nothing replays is worth: **23 frozen strings
//! across 13 files contradicted the tool the product actually registers**,
//! including `schema-org-start-person` freezing "Bring up EXACTLY ONE person"
//! for a tool that takes a list, and `schema-org-remove-contract` freezing a
//! confirmation strictly MORE destructive than the one the product asks for. A
//! blocked fixture that contradicts the product is worse than no fixture,
//! because the diff claims it is covered.
//!
//! # What remains, and why it is worth keeping
//!
//! The 3 here have a live Rust subject: their tools stopped shelling out to
//! `apps/cli` and now cross an HTTP boundary a `/v1/*` route owns, so a fixture
//! records something Rust can be held to. They are replayed by the runners in
//! [`REPLAY_RUNNERS`], and [`the_replayed_fixtures_still_have_a_live_rust_runner`]
//! is the tripwire that fails if one of those runners is deleted.

mod conformance_common;

use conformance_common::{load_fixtures, repo_root};

const FAMILY: &str = "tools";

/// The Rust runners that replay [`REPLAYED_IN_RUST`].
///
/// They live in `chiefd-api` because that is where the routes are; this crate
/// cannot link them, which is why the coupling is a named file plus a tripwire
/// rather than a call.
const REPLAY_RUNNERS: &[&str] = &["apps/chiefd/crates/chiefd-api/tests/conformance_reminders.rs"];

/// Fixtures that DO have a Rust subject, and are replayed against it.
///
/// These are excluded from the blocked accounting below — counting a replayed
/// fixture as blocked would understate the coverage that exists, and the whole
/// point of this file is that its numbers are true.
const REPLAYED_IN_RUST: &[&str] = &[
    "org-create-reminder-arms-a-durable-recurring-reminder-for-yourself",
    "org-list-reminders-lists-your-own-durable-reminders",
    "org-stop-reminder-disarms-one-reminder-and-retains-the-row",
];

/// The fixtures that observe NO durable state, and the exact reason.
///
/// Their only `expectState` entry read `tools.launcher_calls`, the argv a tool
/// spawned `apps/cli` with. #751/G9 deleted that transport from
/// `organization-intercom.ts` AND the scripted host in
/// the deleted `conformance/lib/tool-host.ts` that answered it, leaving an observable whose
/// producer was a `calls` array nothing ever pushed to. 38 of these fixtures
/// pinned a non-empty argv and were false; the rest asserted the empty list and
/// could not fail. #1044 deleted the read rather than re-recording 59 bodies no
/// Rust replays — a fixture with no Rust subject is not a test, whatever it
/// contains, and an invented body would only have been a fresh unverified
/// assertion. The corpus has no working recorder either
/// (the TypeScript harness is deleted; #1046 made the replay runner the
/// recorder), so a regenerated body would be
/// hand-written, not observed.
///
/// This list is what keeps that honest. It is EXACT in both directions: a
/// fixture that asserts nothing and is not named here fails, and a fixture named
/// here that grows an assertion fails too. So the corpus's coverage hole has a
/// size, a membership, and a reason, instead of being invisible.
const NO_OBSERVABLE_STATE: &[&str] = &[];

/// Every fixture states a complete FORMAT.md contract.
///
/// This is the one check that can run today, and it runs on all of them: the
/// family/name/directory agreement and the non-empty description come from
/// `load_fixtures`, and the rest of the contract is asserted here. A fixture
/// that cannot be replayed still has to be well formed — the corpus is the
/// asset the port will be measured against, so it must not rot while it waits.
#[test]
fn every_tools_fixture_states_a_complete_contract() {
    let fixtures = load_fixtures(FAMILY);
    let mut checked = 0_usize;
    for (name, fixture) in &fixtures {
        let expect = &fixture["expect"];
        let ok = expect.get("ok").is_some();
        let error = expect.get("error");
        assert!(ok ^ error.is_some(), "{name}: `expect` must be exactly one of `ok` or `error`");
        if let Some(error) = error {
            assert!(
                matches!(
                    error["type"].as_str(),
                    Some(
                        "Refused"
                            | "Conflict"
                            | "Busy"
                            | "StoreFailure"
                            | "Corrupt"
                            | "Unavailable"
                    )
                ),
                "{name}: '{}' is outside the closed taxonomy",
                error["type"]
            );
            let code = error["code"].as_str().unwrap_or_default();
            assert!(!code.is_empty(), "{name}: a recorded refusal needs a code");
            assert_ne!(
                code, "unclassified",
                "{name}: an unclassified refusal cannot guide the port (FORMAT.md)"
            );
        }

        let setup = fixture["setup"].as_array().expect("`setup` is an array");
        assert_eq!(
            setup.first().and_then(|step| step["op"].as_str()),
            Some("company.create"),
            "{name}: every fixture starts from a fresh, empty world"
        );

        let expect_state = fixture["expectState"].as_array().expect("`expectState` is an array");
        let registered = NO_OBSERVABLE_STATE.contains(&name.as_str());
        assert_eq!(
            expect_state.is_empty(),
            registered,
            "{name}: a fixture must assert durable state unless it is registered in \
             NO_OBSERVABLE_STATE, and a registered fixture must assert none. If this one now \
             observes something real, delete its row; if it lost its last observation, add one \
             with the reason.",
        );

        // FORMAT.md: a `tools.*` op is issued under an identity, because
        // registration IS the authorization surface today.
        if fixture["op"].as_str().is_some_and(|op| op.starts_with("tools.")) {
            let caller = fixture.get("caller").expect("a tools op needs a caller");
            assert!(
                caller["personId"].as_str().is_some_and(|id| !id.is_empty()),
                "{name}: a tools caller needs a personId"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, fixtures.len(), "checked {checked} of {} fixtures on disk", fixtures.len());
    println!("{checked}/{} tools fixtures shape-checked", fixtures.len());
}

/// The replay this file credits still exists, and still names every fixture.
///
/// Without this, `REPLAYED_IN_RUST` is a claim about another crate that nothing
/// checks — delete `conformance_reminders.rs` and three fixtures would quietly
/// stop being both replayed AND blocked, which is worse than either. The needle
/// is each fixture's own name, so the runner cannot satisfy this by existing.
#[test]
fn the_replayed_fixtures_still_have_a_live_rust_runner() {
    let mut named = String::new();
    for runner in REPLAY_RUNNERS {
        let path = repo_root().join(runner);
        named.push_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{runner} is unreadable ({error}), but this file credits it with replaying part \
                 of a {}-fixture set. Either restore it, or move those fixtures back into the \
                 blocked set and say why.",
                REPLAYED_IN_RUST.len()
            )
        }));
    }
    for name in REPLAYED_IN_RUST {
        assert!(
            named.contains(name),
            "no runner in {REPLAY_RUNNERS:?} names '{name}', so this file is crediting a replay \
             that does not happen.",
        );
    }
}
