//! A6. WHICH MOUNT SERVES WITHOUT AN AUTH RUNTIME — and the answer is NONE.
//!
//! The verify-middleware took `State(Option<AuthState>)`, where `None` passed
//! every request through with no caller at all. That arm was never an off
//! switch by intent; it existed because one mount — `chiefd docstore-only` —
//! genuinely had no identities to resolve. It was the shape an off switch
//! would take if one came back, so the mounts that reached it were pinned here
//! by name rather than left to be re-derived.
//!
//! `docstore-only` is deleted, so the set is empty and this file now pins the
//! stronger fact: EVERY serve site in this crate attaches a runtime. A new
//! mount cannot be added without failing this file, which forces its author to
//! say which arm it takes — and there is only one arm left to take.
//!
//! The mechanical half is two counts: how many places start serving, and how
//! many attach a runtime. They must be equal.

/// Every module in this crate that can start serving the HTTP surface.
const DAEMON_SOURCES: &[(&str, &str)] = &[("run.rs", include_str!("../src/run.rs"))];

/// The serve entry points. A mount that does not go through one of these does
/// not serve HTTP at all.
const SERVE_CALLS: &[&str] = &["docstore::serve_bound(", "docstore::serve_bound_with_watch("];

/// Count real call sites of `needles` — never a doc comment, never prose.
fn call_sites(needles: &[&str]) -> Vec<(&'static str, usize)> {
    DAEMON_SOURCES
        .iter()
        .map(|(name, source)| {
            let calls = source
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//") && needles.iter().any(|call| trimmed.contains(call))
                })
                .count();
            (*name, calls)
        })
        .collect()
}

/// THE INVENTORY. Two serve sites, and the classification of each is stated
/// rather than implied.
///
/// * `run.rs`, `Daemon::serve` — the live company surface. Its `Bound` comes
///   from `reservation.mount(…).with_auth(Some(auth_runtime))`, so every
///   non-exempt route requires a bearer.
/// * `run.rs`, `serve_only_snapshot` — `chiefd run --serve-only`. It used to be
///   unauthenticated AND to refuse to start whenever the universal gate was on.
///   A7 (#1114) gave it the same runtime `run_company` builds, from the same
///   `<dir>/.chief/keys` through the same helper, because unlike the deleted
///   `docstore-only` mode it HOLDS the company. A6 then deleted the refusal,
///   whose premise the runtime had already retired.
#[test]
fn the_serve_sites_are_exactly_two_and_each_one_is_classified() {
    let sites = call_sites(SERVE_CALLS);
    let total: usize = sites.iter().map(|(_, count)| count).sum();
    assert_eq!(
        sites,
        vec![("run.rs", 2)],
        "a serve site was added, moved or removed. Classify it in this file's doc comment \
         first: does it attach an auth runtime, or is it a new unauthenticated surface? \
         There are currently none of the second kind, and that is the point."
    );
    assert_eq!(total, 2);
}

/// BOTH of the two attach a runtime, and both pass `Some` with nothing beside
/// it. `with_auth` took an `enforce: bool` second argument until A6 deleted it,
/// so a caller could attach a runtime and still serve an anonymous request.
/// There is no argument left that could.
#[test]
fn every_authenticated_mount_attaches_a_runtime_and_takes_no_second_argument() {
    let mut calls = Vec::new();
    for (name, source) in DAEMON_SOURCES {
        for line in source.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.contains(".with_auth(") {
                calls.push((*name, trimmed.trim_end_matches(',').to_string()));
            }
        }
    }
    assert_eq!(calls.len(), 2, "expected exactly two with_auth call sites, found {calls:?}");
    for (file, call) in &calls {
        assert_eq!(*file, "run.rs");
        assert!(
            call.starts_with(".with_auth(Some(") && call.ends_with("))"),
            "an authenticated mount must attach a runtime with no second argument: {call}"
        );
    }
}

/// THE COUNTS MATCH, which is the whole claim: serve sites and runtime
/// attachments are the same number, so no mount serves without one. This
/// replaces the old `docstore_only_is_the_only_mount_that_serves_with_no_runtime`,
/// whose subject is deleted.
#[test]
fn no_mount_serves_without_a_runtime() {
    let serving: usize = call_sites(SERVE_CALLS).iter().map(|(_, count)| count).sum();
    let attaching: usize = call_sites(&[".with_auth("]).iter().map(|(_, count)| count).sum();
    assert_eq!(
        serving, attaching,
        "every serve site must attach an auth runtime; {serving} serve, {attaching} attach"
    );
}

/// The rollout switch is gone from this crate, in every spelling. A guard on
/// the name is what stops it being reintroduced as a "temporary" escape hatch
/// under the same reasoning that produced it the first time.
#[test]
fn no_rollout_switch_survives_in_the_daemon() {
    for (name, source) in DAEMON_SOURCES {
        for token in ["authn::boot::mode", "AuthMode", "auth_enforce", "AUTH_ENABLED_ENV"] {
            assert!(
                !source.contains(token),
                "{name} still names `{token}`; the universal-gate rollout switch is deleted"
            );
        }
    }
}
