//! The de-duplication is only real if this crate's paths reach the LEAF's
//! definition rather than a local copy of it.
//!
//! # Why the obvious test cannot be written
//!
//! The property one would like to assert is "`chief_cli` and `chiefd_host`
//! name ONE type". No test may say that, because no crate may depend on both:
//! the backend/client boundary guard forbids an edge in either direction, and
//! that prohibition is the whole reason `host-primitives` exists. So the
//! property is pinned in two halves — this file for the client, its twin in
//! `chiefd-host` for the backend — each anchoring its own public paths to the
//! leaf. Two crates that both equal the leaf equal each other.
//!
//! # Why it is worth a test at all
//!
//! Both crates compiling proves nothing here: two crates each holding their
//! own `HostErr`, `Pid` and `ProcIdentity` compiled perfectly well before this
//! packet — that WAS the duplication. A move can also be faked by re-exporting
//! a local copy, and every path in the codebase would still resolve. Handing a
//! value across the paths is what fails if the two ever drift apart again.
//!
//! The moved code's BEHAVIOUR is covered where it now lives, by the tests that
//! moved with it. Re-asserting those bodies here would re-create the
//! duplication this packet exists to remove.

use chief_cli::actuate::host::{HostErr as CliHostErr, Pid as CliPid, ProcIdentity as CliIdentity};

/// Takes the LEAF's spelling. Everything below is handed to it through the
/// client's own paths, so each call is the assertion.
fn leaf_error(error: host_primitives::HostErr) -> host_primitives::HostErr {
    error
}

#[test]
fn the_clients_host_error_is_the_leafs_host_error() {
    let from_client = CliHostErr::Untrusted { reason: "server exited unexpectedly".to_string() };
    let returned = leaf_error(from_client);
    assert!(
        matches!(returned, host_primitives::HostErr::Untrusted { .. }),
        "the client's variant is the leaf's variant, not a look-alike"
    );

    // The widening this packet disclosed, exercised rather than described: the
    // reason is OWNED, so an actuator's wire prose fits where the client used
    // to require a compile-time literal — and nothing is discarded.
    let from_wire = String::from("actuator said so at runtime");
    let wire = CliHostErr::Untrusted { reason: from_wire.clone() };
    let host_primitives::HostErr::Untrusted { reason } = leaf_error(wire) else {
        panic!("crossing the path does not change the variant");
    };
    assert_eq!(reason, from_wire);
}

#[test]
fn the_clients_pid_and_proc_identity_are_the_leafs() {
    let pid: CliPid = host_primitives::Pid(4_242);
    let back: host_primitives::Pid = pid;
    assert_eq!(back.0, 4_242);

    // `start_time` is the field that defeats pid recycling, so a drifted copy
    // here would be a silently weaker identity check rather than a type error
    // anybody notices.
    let identity: CliIdentity =
        host_primitives::ProcIdentity { pid: host_primitives::Pid(7), start_time: 99 };
    let returned: host_primitives::ProcIdentity = identity;
    assert_eq!(returned.pid.0, 7);
    assert_eq!(returned.start_time, 99);
}

/// Layer 3's vocabulary, pinned the same way and for the same reason.
///
/// `MaterializePlan` is JOURNALLED before the filesystem is touched, so a crash
/// recovery replays it — which makes a drifted second copy worse than a
/// duplicated type: the two sides would disagree about what a recovery record
/// MEANS while both still compiling.
#[test]
fn the_clients_materialize_vocabulary_is_the_leafs() {
    let plan: chief_cli::actuate::host::MaterializePlan =
        host_primitives::materialize::MaterializePlan {
            root: std::path::PathBuf::from("/srv/orgs/acme"),
            files: vec![host_primitives::materialize::MaterializeFile {
                relative_path: "people/ada/AGENTS.md".to_string(),
                contents: "mandate".to_string(),
                mode: 0o644,
            }],
        };
    let back: host_primitives::materialize::MaterializePlan = plan;
    assert_eq!(back.files.len(), 1);
    assert_eq!(back.files[0].mode, 0o644, "the publish mode survives the seam");

    let drift: chief_cli::actuate::host::DriftReport = host_primitives::materialize::DriftReport {
        changed: vec!["a".to_string()],
        unchanged: Vec::new(),
        conflicts: Vec::new(),
    };
    let back: host_primitives::materialize::DriftReport = drift;
    assert_eq!(back.changed, vec!["a".to_string()]);
}
