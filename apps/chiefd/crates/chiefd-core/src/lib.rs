//! `chiefd-core` — the deterministic half of chiefd.
//!
//! Everything in this crate is pure with respect to the host: it owns durable
//! state (SQLite), the single-writer actor that mutates it, and the
//! failure-polarity type machinery. It performs **no** the runtime, pi, or filesystem side effects — those
//! live behind [`chiefd-host`]'s `HostExecutor`, which is the unit-test seam
//! (plan §4).
//!
//! Module ownership (plan §9, Track B):
//!
//! | Module | Owns | Milestone |
//! |---|---|---|
//! | [`clock`] | The injected `Clock` trait every TTL/ladder/deadline flows through | M1 |
//! | [`diagnostics`] | The ported `boundedPersistedError` bound/redaction every stored diagnostic passes | M10 |
//! | [`error`] | `ChiefdError`, including the unmintable `Busy` | M1 |
//! | [`host_action`] | The `host_actions` journal row: the DB half of the DB↔filesystem 2PC | M9 |
//! | [`isotime`] | ISO-8601 rendering/parsing for the ported ledger timestamp fields | M10 |
//! | [`ledger`] | The in-memory working set, the committed snapshot, `validate()` | M4 |
//! | [`polarity`] | The three failure markers, the sealed store registry macro, the (store × op) matrix | M7 |
//! | [`schema`] | SQL DDL, migrations, `validate(&Ledger)` hooks | M4 |
//! | [`store`] | The store registry and per-store ledgers; the **only** place a `rusqlite::Connection` is opened | M4/M7/M10/M12 |
//! | [`actor`] | The per-company writer thread, `mutate`/`read`, the class scheduler | M4 |
//!
//! # Scaffold status
//!
//! This is the M1 scaffold. The types below fix the shapes the later
//! milestones fill in; where a body is not yet written it is stated in the doc
//! comment rather than left as a silent stub.
//!
//! [`chiefd-host`]: https://docs.rs/chiefd-host

#![forbid(unsafe_code)]

pub mod actor;
pub mod clock;
pub mod diagnostics;
pub mod error;
pub mod hexdigest;
pub mod host_action;
pub mod isotime;
pub mod ledger;
pub mod polarity;
pub mod runtime;
pub mod schema;
pub mod store;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use error::{ChiefdError, Refusal};

/// Schema version chiefd reports on `GET /v1/health` (plan §7.1). `chiefctl`
/// compares it against its own; a mismatch is a hard error, not a warning.
pub const SCHEMA_VERSION: u32 = 1;

/// Whether this build has the test-only surface compiled in.
///
/// CI asserts this is `false` for the release artifact (plan §5.2 item 3):
/// the zero-wait retry ladder and the manual clock must never reach a live
/// company.
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
