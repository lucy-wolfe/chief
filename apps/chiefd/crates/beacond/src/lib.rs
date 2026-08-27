//! `beacond` — the box-wide company registry.
//!
//! Its 100%-sole job is to be the durable registry of companies (ruling
//! D21): which companies exist on this box, and where each one's chiefd is
//! currently running. It owns one SQLite table, `companies`, keyed by the
//! DIRECTORY each company occupies, and serves eight HTTP routes with **no
//! authentication**.
//!
//! **It is no longer on the path between a command and its own company.** A
//! command standing in a directory finds that directory's daemon by reading
//! `<dir>/.chief/run/daemon.json` (`host_primitives::rendezvous`), because a
//! directory already knows where its own daemon is. What survives here is
//! the one question no single directory can answer — what is running
//! anywhere on this machine — which is what `chief ls` and `apps/web` ask.
//!
//! **No auth is the shipped design, not a placeholder.** The right shape now
//! (mandate 0) for a service every local chiefd must reach with no
//! bootstrap credential is an unauthenticated loopback listener: any local
//! process can register or deregister any directory. That is acceptable while
//! every chiefd and every client is the same user on the same box behind a
//! loopback bind (the default `127.0.0.1:6969`); it is not acceptable the
//! day beacond ever binds a routable address, which is a different service
//! with a different threat model — not an upgrade to this one.
//!
//! **No background loop.** beacond never expires, sweeps or reaps a row on
//! a timer — see [`store`] and [`liveness`].
//!
//! This crate is both a library and a binary (`src/main.rs`), the same
//! shape `chiefd-api` uses in this workspace: the library is what the
//! `tests/` integration suite links against (a bin-only crate cannot be
//! imported by its own integration tests), and `main.rs` is thin wiring
//! over it.

#![forbid(unsafe_code)]

pub mod config;
pub mod liveness;
pub mod router;
pub mod shutdown;
pub mod store;
pub mod watchdog;
pub mod wire;
