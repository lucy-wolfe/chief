//! The backend half of the one-definition property.
//!
//! Its twin is `chief-cli/tests/one_host_vocabulary.rs`, and the split is
//! forced rather than stylistic: the property worth asserting is "`chief_cli`
//! and `chiefd_host` name ONE type", and no test may say that, because no
//! crate may depend on both — the backend/client boundary guard forbids an
//! edge in either direction, which is the whole reason `host-primitives`
//! exists. Each half anchors its own crate's public paths to the leaf, and two
//! crates that both equal the leaf equal each other.
//!
//! Compiling is not the assertion. Two crates each holding their own `HostErr`
//! compiled perfectly well before this packet — that WAS the duplication — and
//! a move can be faked by re-exporting a local copy with every path still
//! resolving. Handing values across the paths is what fails if they drift.

use chiefd_host::executor::{HostErr, Pid, ProcIdentity};

fn leaf_error(error: host_primitives::HostErr) -> host_primitives::HostErr {
    error
}

#[test]
fn the_backends_host_error_is_the_leafs_host_error() {
    // The backend is the side whose untrusted reasons arrive as prose on the
    // wire, and the owned `String` is the shape that survived the merge for
    // exactly that reason. Narrowing it would have discarded this value.
    let from_wire = String::from("actuator said so at runtime");
    let error = HostErr::Untrusted { reason: from_wire.clone() };
    let host_primitives::HostErr::Untrusted { reason } = leaf_error(error) else {
        panic!("crossing the path does not change the variant");
    };
    assert_eq!(reason, from_wire, "no reason is discarded by the shared shape");
}

#[test]
fn the_backends_pid_and_proc_identity_are_the_leafs() {
    let pid: Pid = host_primitives::Pid(4_242);
    let back: host_primitives::Pid = pid;
    assert_eq!(back.0, 4_242);

    // `start_time` is what defeats pid recycling, so a drifted copy here would
    // be a silently weaker identity check rather than a loud type error.
    let identity: ProcIdentity =
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
fn the_backends_materialize_vocabulary_is_the_leafs() {
    let plan: chiefd_host::executor::MaterializePlan =
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

    let drift: chiefd_host::executor::DriftReport = host_primitives::materialize::DriftReport {
        changed: vec!["a".to_string()],
        unchanged: Vec::new(),
        conflicts: Vec::new(),
    };
    let back: host_primitives::materialize::DriftReport = drift;
    assert_eq!(back.changed, vec!["a".to_string()]);
}
