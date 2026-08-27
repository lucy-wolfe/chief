//! The operator client's placement engine: pure, testable, and dependency-free.
//!
//! # Why this crate has a library as well as a binary
//!
//! Placement is arithmetic over published facts — it opens no socket, spawns
//! no process and reads no environment. Keeping it behind a library boundary
//! is what lets it be exercised as a value rather than as a program: the
//! shared golden `apps/chiefd/tests/fixtures/placement-golden.json` is
//! computed here and asserted byte-identical against the answer chiefd still
//! computes on its own side of the split, and neither side links the other.
//! `crates/beacond` already has exactly this shape, for the same reason.
//!
//! The binary ([`main.rs`](../src/main.rs)) keeps everything that touches the
//! world — the HTTP transport, beacond discovery, the tmux probe, the listing
//! — and consumes this library for the answer.
//!
//! #751/P8 added a second, impure half: [`actuate`], which holds everything
//! that knows what a pane is. It moved here wholesale from `chiefd-host`
//! because its seam is an EMULATION seam — tmux argv in, tmux exit codes and
//! literal tmux stderr out — so nothing that does not speak tmux could ever
//! have sat behind it in the backend.
//!
//! # The one line the whole crate exists to hold
//!
//! **chiefd decides WHO runs. The client decides WHERE it is displayed.** This
//! crate is the second half, and it links none of `chiefd-core`,
//! `chiefd-host`, `chiefd-api` or `chiefd` — enforced in both directions by
//! `scripts/test/backend-tmux-boundary.test.mjs`. Every type in [`roster`] is
//! a second declaration written against the JSON, never an import.

#![forbid(unsafe_code)]

pub mod actuate;
// The one reader of `/run/tribes-theme`. It is a top-level module rather than
// a helper inside the rail because the rail is no longer its only caller: the
// actuator seeds the same answer into every pane's launch environment, and a
// second derivation of one authority is how a light rail ends up beside a dark
// pane.
pub mod appearance;
// The operator client's half of agent-auth. It lives in the LIBRARY half
// rather than beside the transport that consumes it because BOTH halves need
// it: the binary's `http::Client` signs as the operator, and [`actuate`]'s
// resident client signs as the service identity — and `actuate` cannot name a
// module `main.rs` declares. One acquirer, one failure policy, one cache.
pub mod bearer;
// Stage 0 of that work: the harness that times a click to the
// pixels it produces, rather than to a command being issued.
pub mod bench;
pub mod control;
pub mod files;
// The one log shape every bounded wait in this client emits. It lives in the
// library rather than beside its callers because those callers straddle the
// lib/bin split: `actuate::exec`'s transient ladder is here, and `attach`'s two
// session waits are in the binary. A second copy of "a poll that is expected to
// miss is not a warning" is how one of them drifts back to warning.
pub mod ladder;
pub mod layout;
pub mod lifecycle;
pub use host_primitives::pause;
pub mod placement;
// Layer 2 of the design record: `proc` and `spawn` are the leaf's
// now, along with the `HostErr`/`Pid`/`ProcIdentity` vocabulary they are
// written against. Re-exported at their old paths, so `chief_cli::proc::
// ProcReader` and `crate::spawn::spawn_detached` are unchanged for every
// caller, the crash tests under `tests/` included.
pub use host_primitives::proc;
pub mod real;
// Everything a company's panes started, and how a stop ends it. In the LIBRARY
// half because it is pure signalling arithmetic over a list of pids the binary
// reads from tmux — it opens nothing and knows no company — so it can be
// exercised against real process groups as a value rather than as a program.
pub mod reap;
pub mod roster;
/// The operator's sidebar rail: Departments over People, in one tmux pane.
pub mod sidebar;
pub use host_primitives::spawn;
pub(crate) mod window_geometry;
