//! `chiefd-host` — everything chiefd does *to the machine*.
//!
//! Plan §4: the genuine boundary is not process vs. daemon, it is chiefd's
//! store actors (pure, deterministic, fake-able) versus its side-effectful
//! host module. The `HostExecutor` trait was that boundary and therefore the
//! unit-test seam: store-layer tests run against a fake, and the real runtime
//! behaviour is exercised only by this crate's own tests and the e2e harness
//! (TESTING.md §1.3).
//!
//! Module ownership (plan §9, Track C):
//!
//! | Module | Owns | Milestone |
//! |---|---|---|
//! | [`files`] | Publish-by-rename, materialization | M5 |
//! | [`host_txn`] | The DB↔filesystem 2PC: intent → publish → close, plus startup replay | M9 |
//! | [`verified_export`] | Write-then-verify JSON exports: a changed-underneath file is a hard stop, never an overwrite | M11 |
//! | [`provider_credentials`] | The `notify`-backed root-registry watcher; per-person credential/auth.json re-staging (E8-S0, #822) | E8 |
//!
//! # What is NOT here any more (#751/P8-P10)
//!
//! The `HostExecutor` trait, `RealHostExecutor`, the pane vocabulary, the trust
//! rules, `/proc` identity, the provider probe, the pane-launch command builder
//! and the named pause points have all MOVED to `chief-cli`'s `actuate` tree.
//! P9 renamed them in place rather than moving them — the multiplexer's own
//! prefix became `Runtime*`
//! while every file stayed put, which turned the boundary guard green without
//! moving a line — so P10 performs the move the rename stood in for. There is
//! no re-export left here: a backend crate that can still name a pane is the
//! defect, and an alias is a way to name one.
//!
//! chiefd is **host-local by hard commitment** (plan §10 Q3): UDS transport,
//! `/proc` authentication and an in-process executor. If the fleet ever spans
//! machines, this trait is the seam — but auth and liveness would need a
//! redesign, and the UDS design does not stretch.

#![recursion_limit = "512"]
#![forbid(unsafe_code)]

/// `ensure_agent_home` — create-if-absent, never modify. The whole of chief's
/// involvement in an agent's home, replacing the materializer that reprojected
/// every home from SQL on every pass.
pub mod accent;
pub mod agent_home;
mod agent_theme;
pub mod converge_apply;
// Layer 2 of the design record: `proc` and `spawn` are the leaf's
// now, along with the `HostErr`/`Pid`/`ProcIdentity` vocabulary they are
// written against, and are re-exported here at their old paths.
//
// #751/P8-P10: `executor`, `real`, `pi`, `proc`, `redact` and `pause` are back —
// but as the NON-PANE half only. The pane half (the `HostExecutor` display
// verbs, `RuntimeHost`, the runner/waiter seam, the trust classifier and the
// scripted fake) moved to `chief-cli` and is not re-exported from here in any
// form,
// because a backend crate that can still name a pane is the defect and an alias
// is a way to name one.
pub mod executor;
// TOMBSTONE: `extension_drift`. It compared a person's COPIED
// `pi-home/extensions` files against the checkout that produced them, and
// carried the `.organization-reload-adoption` sentinel a live agent wrote to
// say it had reloaded. Nothing is copied into a person's home any more, so the
// comparison has no left-hand side and the sentinel has nothing to adopt. A
// deploy still replaces every affected pane, by `materialize::
// extension_source_digest` moving `desired_launch_hash` — a construction, not a
// scan.
pub mod files;
pub mod gather;
pub mod host_txn;
pub mod identity_enrolment;
pub mod identity_key;
pub mod materialize;
pub mod person_presentation;
pub mod project_skills;
pub use host_primitives::pause;
pub use host_primitives::proc;
pub mod real;
pub use host_primitives::redact;
pub mod runtime_lifecycle;
pub mod runtime_waker;
pub use host_primitives::spawn;
pub mod verified_export;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

// The re-export list is deliberately short, and deliberately has no pane in it.
// It used to read `pub use executor::{HostErr, HostExecutor, PaneId, Pid,
// Socket};` — a backend crate publishing a pane id and a display socket as part of
// its public surface. `PaneId` and `Socket` are `chief-cli`'s vocabulary now and
// are not re-exported from here in any spelling, because an alias is a way to
// name a pane and naming one is the thing this crate must not be able to do.
pub use executor::{HostErr, HostExecutor, Pid};
pub use real::RealHostExecutor;

pub use runtime_waker::{
    DeferToInterval, NotifyReconcileTrigger, ReconcileTrigger, ReconcileWaker,
};
