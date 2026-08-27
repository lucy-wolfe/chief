//! Host-side gatherers — the seam between chiefd's pure input structs and the
//! real machine.
//!
//! Half-2 of the one-daemon port splits every duty into a **pure core** (a
//! data-in/data-out function in `chiefd-core`, fake-able and red/green
//! testable) and a **host gatherer** (here) that assembles the core's input by
//! actually observing the runtime, `/proc` and the durable documents. The pure cores
//! for the D9 supervision cycle
//! ([`cycle`](chiefd_core::store::supervision::cycle)) and the health-monitor
//! collection pass ([`collect`](chiefd_core::store::health_collect::collect))
//! already exist; this module is the gathering step they were missing.
//!
//! # What "host" means here, precisely
//!
//! A gatherer's input struct mixes two kinds of fact, and the split is
//! deliberate:
//!
//! * **Durable facts** — fleet suppression, runtime ownership, the desired
//!   projection, the supervisor/runtime/effect documents — are read
//!   from SQLite by the store layer and handed to the gatherer as
//!   already-observed values. This mirrors
//!   [`ObserveInput`](chiefd_core::store::supervision::ObserveInput), which
//!   takes `suppressed`/`foreign_holder` as given rather than gathering them:
//!   the gatherer does not open a database.
//! * **Host facts** — the runtime ownership audit, dead panes, and the
//!   stale-generation health diff — are the genuine machine reads, taken here
//!   through [`HostExecutor`](crate::executor::HostExecutor) so a fake host
//!   drives them in tests.
//!
//! # Observation failed is not observation empty
//!
//! Every gatherer fails **closed**. A runtime read that could not be trusted
//! ([`HostErr::Untrusted`](crate::executor::HostErr::Untrusted)) propagates as
//! an error; it is never flattened into an empty audit or an all-healthy
//! sample, because "we could not see" and "we saw nothing" drive opposite
//! supervision decisions and confusing them is exactly how a transient hiccup
//! becomes a wrongful takeover or a missed incident.

pub mod cycle_input;
pub mod health_snapshot;
pub mod reconciler_facts;

pub use cycle_input::{gather_cycle_input, CycleGatherContext, HostCycleInputGatherer};
pub use health_snapshot::{
    gather_health_snapshot, HealthGatherContext, HostHealthSnapshotGatherer,
};
pub use reconciler_facts::ReconcilerFactsStore;
