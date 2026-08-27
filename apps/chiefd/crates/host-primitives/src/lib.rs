//! Host primitives both actuators share.
//!
//! # Why this is a crate and not a module in one of the others
//!
//! Two crates run host effects and need exactly the same answers:
//!
//! * **`chiefd-host`** — the backend actuator.
//! * **`chief-cli`** — the operator client, installed as `chiefd`.
//!
//! They cannot share a module through either crate, because `chief-cli` is
//! forbidden to depend on the backend crates and the backend is forbidden to
//! depend on it (the backend/client boundary guard, rules 5 and 7). So the
//! shared definition lives in a leaf both may link.
//!
//! The disagreement this closes has not happened here YET, and that is the
//! point of moving these two members first: they are the ones whose copies
//! are still identical. The class is not hypothetical — the `kill(pid, 0)`
//! liveness judge was written four times in this workspace and two copies
//! read `EPERM` as death, and the frontmatter gate was written
//! three times and gave three different verdicts on the same document. Both
//! were found only after they had drifted.
//!
//! Layer 2 found the third case, and it had already started: the two copies of
//! [`HostErr`] agreed on all four variant names and disagreed on one field
//! type. See [`error`] for which shape survived and why.
//!
//! # What this crate does not own
//!
//! Anything that would drag a shared TYPE across the boundary that has not
//! been moved yet. Layer 2 moved the first of them — [`HostErr`], [`Pid`] and
//! [`ProcIdentity`], and with them [`proc`] and [`spawn`], neither of which
//! could compile without them. Layer 3 moved the materialization vocabulary,
//! [`materialize`], which is what this crate's `serde` dependency was spent on.
//! `files` and `real` are still mirrored:
//!
//! * `files` is written against `MaterializePlan` and `DriftReport`, which are
//!   now here — so the type barrier is gone and what remains is the
//!   publish-by-rename seam itself. Layer 3 scoped it out deliberately rather
//!   than half-move it; the design record states the reason.
//! * `real` is NOT a duplicate and must never be collapsed. The client's is
//!   generic over a multiplexer runner and a waiter; the backend's was
//!   de-parameterized by #751/P8-P10 and names no multiplexer.
//!
//! Neither `HostExecutor` trait is here either, for the same reason: they are
//! two different traits that happen to share a name. The client's speaks panes
//! and sockets, the backend's speaks provider probes, and merging them would
//! put a display verb back on the backend — which is the exact regression the
//! boundary guard exists to stop.
//!
//! See the design record, the design record and
//! the design record for the order and what each step costs.

#![forbid(unsafe_code)]

pub mod error;
pub mod identity;
pub mod install;
pub mod materialize;
pub mod pause;
pub mod pi_floor;
pub mod proc;
pub mod redact;
pub mod rendezvous;
pub mod spawn;

pub use error::HostErr;
pub use identity::{Pid, ProcIdentity};
